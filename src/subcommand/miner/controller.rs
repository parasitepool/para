use super::*;

pub(crate) struct Controller {
    client: Client,
    pool_difficulty: Arc<Mutex<Difficulty>>,
    extranonce1: Extranonce,
    extranonce2_size: u32,
    share_rx: mpsc::Receiver<(Header, Extranonce, String)>,
    share_tx: mpsc::Sender<(Header, Extranonce, String)>,
    cancel: CancellationToken,
    current_mining_cancel: Option<CancellationToken>,
    cpu_cores: usize,
    extranonce2_counters: Vec<u32>,
    once: bool,
}

impl Controller {
    pub(crate) async fn new(
        mut client: Client,
        cpu_cores: Option<usize>,
        once: bool,
    ) -> Result<Self> {
        let (subscribe, _, _) = client.subscribe().await?;
        client.authorize().await?;

        let num_cores = cpu_cores.unwrap_or_else(system_utils::get_cpu_count);

        info!(
            "Authorized: extranonce1={}, extranonce2_size={}",
            subscribe.extranonce1, subscribe.extranonce2_size
        );

        info!("Initializing Rayon thread pool with {} cores", num_cores);

        rayon::ThreadPoolBuilder::new()
            .num_threads(num_cores)
            .thread_name(|index| format!("rayon-miner-{}", index))
            .panic_handler(|_| {
                error!("Rayon thread panicked during mining operation");
            })
            .build_global()
            .map_err(|e| anyhow!("Failed to initialize Rayon thread pool: {}", e))?;

        let (share_tx, share_rx) = mpsc::channel(32);

        Ok(Self {
            client,
            pool_difficulty: Arc::new(Mutex::new(Difficulty::default())),
            extranonce1: subscribe.extranonce1,
            extranonce2_size: subscribe.extranonce2_size,
            share_rx,
            share_tx,
            cancel: CancellationToken::new(),
            current_mining_cancel: None,
            cpu_cores: num_cores,
            extranonce2_counters: vec![0; num_cores],
            once,
        })
    }

    pub(crate) async fn run(mut self) -> Result {
        info!(
            "Controller started with {} CPU cores configured",
            self.cpu_cores
        );

        loop {
            tokio::select! {
                Some(msg) = self.client.incoming.recv() => {
                     match msg {
                        Message::Notification { method, params } => {
                            self.handle_notification(method, params).await?;
                        }
                        Message::Request { id, method, params } => {
                            self.handle_request(id, method, params).await?;
                        }
                        _ => warn!("Unexpected message on incoming: {:?}", msg)
                    }
                },
                Some((header, extranonce2, job_id)) = self.share_rx.recv() => {
                    info!("Valid share found: nonce={}, hash={:?}", header.nonce, header.block_hash());

                    if let Err(e) = self.client.submit(job_id, extranonce2, header.time.into(), header.nonce.into()).await {
                        warn!("Failed to submit share: {e}");
                    } else {
                        info!("Share submitted successfully!");
                    }

                    if self.once {
                        info!("Share found, exiting");
                        break;
                    }
                }
                _ = ctrl_c() => {
                    info!("Shutting down controller and mining operations");
                    break;
                }
            }
        }

        self.client.disconnect().await?;

        Ok(())
    }

    async fn handle_notification(&mut self, method: String, params: Value) -> Result {
        match method.as_str() {
            "mining.notify" => {
                let notify = serde_json::from_value::<Notify>(params)?;

                self.cancel_current_mining();

                let network_nbits: CompactTarget = notify.nbits.into();
                let network_target: Target = network_nbits.into();
                let pool_target = self.pool_difficulty.lock().await.to_target();

                info!("New job received: {}", notify.job_id);
                info!(
                    "Network target:\t{}",
                    crate::target_as_block_hash(network_target)
                );
                info!("Pool target:\t{}", crate::target_as_block_hash(pool_target));
                info!("Spawning hasher thread");

                let mining_cancel = CancellationToken::new();
                self.current_mining_cancel = Some(mining_cancel.clone());

                let share_tx = self.share_tx.clone();

                info!(
                    "Starting parallel mining across {} CPU cores",
                    self.cpu_cores
                );

                let nonce_range_per_core = u32::MAX / self.cpu_cores as u32;

                for core_id in 0..self.cpu_cores {
                    let extranonce2_str = self.generate_extranonce2_for_core(core_id);
                    let extranonce2: Extranonce = extranonce2_str
                        .parse()
                        .map_err(|e| anyhow!("Failed to parse extranonce2: {}", e))?;

                    let start_nonce = core_id as u32 * nonce_range_per_core;
                    let end_nonce = if core_id == self.cpu_cores - 1 {
                        u32::MAX
                    } else {
                        (core_id + 1) as u32 * nonce_range_per_core
                    };

                    #[allow(clippy::useless_conversion)]
                    let mut hasher = Hasher::new(
                        Header {
                            version: notify.version.into(),
                            prev_blockhash: notify.prevhash.clone().into(),
                            merkle_root: stratum::merkle_root(
                                &notify.coinb1,
                                &notify.coinb2,
                                &self.extranonce1,
                                &extranonce2,
                                &notify.merkle_branches,
                            )?
                            .into(),
                            time: notify.ntime.into(),
                            bits: notify.nbits.into(),
                            nonce: start_nonce,
                        },
                        pool_target,
                        extranonce2,
                        notify.job_id.clone(),
                    );

                    let share_tx_clone = share_tx.clone();
                    let mining_cancel_clone = mining_cancel.clone();

                    info!(
                        "Starting hasher for core {} with extranonce2: {}, nonce range: {}-{}",
                        core_id, hasher.extranonce2, start_nonce, end_nonce
                    );

                    rayon::spawn(move || {
                        match hasher.hash_with_range(mining_cancel_clone, start_nonce, end_nonce) {
                            Ok(share) => {
                                info!(
                                    "Mining completed successfully on core {}, sending share",
                                    core_id
                                );
                                futures::executor::block_on(async {
                                    if let Err(e) = share_tx_clone.send(share).await {
                                        error!(
                                            "Failed to send found share from core {}: {e:#}",
                                            core_id
                                        );
                                    }
                                });
                            }
                            Err(e) => {
                                if e.to_string().contains("cancelled") {
                                    info!(
                                        "Mining operation was cancelled on core {} (new job received)",
                                        core_id
                                    );
                                } else {
                                    warn!("Mining failed on core {}: {e:#}", core_id);
                                }
                            }
                        }
                    });
                }
            }
            "mining.set_difficulty" => {
                let difficulty = serde_json::from_value::<SetDifficulty>(params)?.difficulty();
                *self.pool_difficulty.lock().await = difficulty;
                info!("Updated pool difficulty: {difficulty}");

                self.log_difficulty_info(difficulty).await;
            }
            _ => warn!("Unhandled notification: {}", method),
        }

        Ok(())
    }

    async fn handle_request(&self, id: Id, method: String, params: Value) -> Result {
        info!("Received request: method={method} id={id} params={params}");
        Ok(())
    }

    fn cancel_current_mining(&mut self) {
        if let Some(cancel_token) = &self.current_mining_cancel {
            let cancel_start = std::time::Instant::now();
            cancel_token.cancel();
            info!(
                "Cancelled current mining operation for new job (cancellation issued in {:?})",
                cancel_start.elapsed()
            );
        }
        self.current_mining_cancel = None;
    }

    async fn shutdown_mining(&mut self) {
        info!("Shutting down all mining operations");

        self.cancel_current_mining();

        self.cancel.cancel();

        tokio::time::sleep(Duration::from_millis(100)).await;

        info!("Mining shutdown complete");
    }

    fn generate_extranonce2_for_core(&mut self, core_id: usize) -> String {
        let counter = self.extranonce2_counters[core_id];
        self.extranonce2_counters[core_id] = counter.wrapping_add(1);

        let extranonce2_bytes = self.extranonce2_size as usize;
        let mut bytes = vec![0u8; extranonce2_bytes];

        bytes[0] = core_id as u8;

        let counter_bytes = counter.to_le_bytes();
        let copy_len = std::cmp::min(counter_bytes.len(), extranonce2_bytes.saturating_sub(1));
        if copy_len > 0 {
            bytes[1..1 + copy_len].copy_from_slice(&counter_bytes[..copy_len]);
        }

        hex::encode(bytes)
    }

    async fn log_difficulty_info(&self, difficulty: Difficulty) {
        let target = difficulty.to_target();
        let difficulty_num = difficulty.0 as f64;

        let estimated_hashes = 2u64.pow(32) as f64 / difficulty_num;

        info!("Difficulty metrics:");
        info!("  - Difficulty: {:.2}", difficulty_num);
        info!("  - Target: {}", crate::target_as_block_hash(target));
        info!("  - Est. hashes for share: {:.0}", estimated_hashes);
    }
}
