use super::*;

pub(crate) struct Controller {
    client: Client,
    pool_difficulty: Arc<Mutex<Difficulty>>,
    extranonce1: Extranonce,
    extranonce2_size: u32,
    share_rx: mpsc::Receiver<(JobId, Header, Extranonce)>,
    share_tx: mpsc::Sender<(JobId, Header, Extranonce)>,
    cancel: CancellationToken,
    cpu_cores: usize,
    extranonce2_counters: Vec<u32>,
    once: bool,
    username: String,
    shares: Vec<Share>,
}

impl Controller {
    pub(crate) async fn new(
        mut client: Client,
        cpu_cores: usize,
        once: bool,
        username: String,
    ) -> Result<Self> {
        let (subscribe, _, _) = client.subscribe().await?;
        client.authorize().await?;

        info!(
            "Authorized: extranonce1={}, extranonce2_size={}",
            subscribe.extranonce1, subscribe.extranonce2_size
        );

        info!("Controller initialized with {} CPU cores", cpu_cores);

        let (share_tx, share_rx) = mpsc::channel(32);

        Ok(Self {
            client,
            pool_difficulty: Arc::new(Mutex::new(Difficulty::default())),
            extranonce1: subscribe.extranonce1,
            extranonce2_size: subscribe.extranonce2_size,
            share_rx,
            share_tx,
            cancel: CancellationToken::new(),
            cpu_cores,
            extranonce2_counters: vec![0; cpu_cores],
            once,
            username,
            shares: Vec::new(),
        })
    }

    pub(crate) async fn run(mut self) -> Result<Vec<Share>> {
        loop {
            tokio::select! {
                _ = ctrl_c() => {
                    info!("Shutting down controller and mining operations");
                    break;
                },
                maybe = self.client.incoming.recv() => match maybe {
                    Some(msg) => {
                        match msg {
                            Message::Notification { method, params } => {
                                self.handle_notification(method, params).await?;
                            }
                            Message::Request { id, method, params } => {
                                self.handle_request(id, method, params).await?;
                            }
                            _ => warn!("Unexpected message on incoming: {:?}", msg)
                        }
                    }
                    None => {
                        info!("Incoming closed, shutting down");
                        break;
                    }
                },
                maybe = self.share_rx.recv() => match maybe {
                    Some((job_id, header, extranonce2)) => {
                        info!("Valid share found: nonce={}, hash={:?}", header.nonce, header.block_hash());


                        let share = Share {
                            extranonce1: self.extranonce1.clone(),
                            extranonce2: extranonce2.clone(),
                            job_id,
                            nonce: header.nonce.into(),
                            ntime: header.time.into(),
                            username: self.username.clone(),
                            version_bits: None,
                        };

                        self.shares.push(share);

                        match self.client.submit(job_id, extranonce2, header.time.into(), header.nonce.into()).await {
                            Err(err) => warn!("Failed to submit share: {err}"),
                            Ok(_) => info!("Share submitted successfully"),
                        }

                        if self.once {
                            info!("Share found, exiting");
                            break;
                        }
                    }
                    None => {
                        info!("Share channel closed");
                        break;
                    }
                }
            }
        }
        
        self.cancel();
        self.client.disconnect().await?;

        Ok(self.shares)
    }

    async fn handle_notification(&mut self, method: String, params: Value) -> Result {
        match method.as_str() {
            "mining.notify" => {
                let notify = serde_json::from_value::<Notify>(params)?;

                self.cancel();

                let network_nbits: CompactTarget = notify.nbits.into();
                let network_target: Target = network_nbits.into();
                let pool_target = self.pool_difficulty.lock().await.to_target();

                info!("New job received: {}", notify.job_id);
                info!("Network target:\t{}", target_as_block_hash(network_target));
                info!("Pool target:\t\t{}", target_as_block_hash(pool_target));

                let mining_cancel = CancellationToken::new();

                let share_tx = self.share_tx.clone();

                info!(
                    "Starting parallel mining across {} CPU cores",
                    self.cpu_cores
                );

                for core_id in 0..self.cpu_cores {
                    let extranonce2 = self.generate_extranonce2_for_core(core_id);

                    let mut hasher = Hasher {
                        header: Header {
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
                            nonce: 0,
                        },
                        pool_target,
                        extranonce2: extranonce2.clone(),
                        job_id: notify.job_id,
                    };

                    let share_tx_clone = share_tx.clone();
                    let mining_cancel_clone = mining_cancel.clone();

                    info!(
                        "Starting hasher for core {} with extranonce2: {}",
                        core_id, extranonce2
                    );

                    tokio::spawn(async move {
                        let (tx, rx) = tokio::sync::oneshot::channel();

                        rayon::spawn(move || {
                            let result = hasher.hash(mining_cancel_clone);
                            let _ = tx.send(result);
                        });

                        match rx.await {
                            Ok(Ok(share)) => {
                                info!(
                                    "Mining completed successfully on core {}, sending share",
                                    core_id
                                );
                                if let Err(e) = share_tx_clone.send(share).await {
                                    error!(
                                        "Failed to send found share from core {}: {e:#}",
                                        core_id
                                    );
                                }
                            }
                            Ok(Err(e)) => {
                                if e.to_string().contains("cancelled") {
                                    info!(
                                        "Mining operation was cancelled on core {} (new job received)",
                                        core_id
                                    );
                                } else {
                                    warn!("Mining failed on core {}: {e:#}", core_id);
                                }
                            }
                            Err(e) => {
                                error!(
                                    "Failed to receive mining result from core {}: {e:#}",
                                    core_id
                                );
                            }
                        }
                    });
                }
            }
            "mining.set_difficulty" => {
                let difficulty = serde_json::from_value::<SetDifficulty>(params)?.difficulty();
                *self.pool_difficulty.lock().await = difficulty;
                info!("Updated pool difficulty: {difficulty}");
                info!(
                    "Updated pool target:\t{}",
                    target_as_block_hash(difficulty.to_target())
                );
            }
            _ => warn!("Unhandled notification: {}", method),
        }

        Ok(())
    }

    async fn handle_request(&self, id: Id, method: String, params: Value) -> Result {
        info!("Received request: method={method} id={id} params={params}");
        Ok(())
    }

    fn cancel(&mut self) {
        self.cancel.cancel();
        self.cancel = CancellationToken::new();
    }

    fn generate_extranonce2_for_core(&mut self, core_id: usize) -> Extranonce {
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

        Extranonce::from_hex(&hex::encode(bytes)).expect("Valid extranonce2")
    }
}
