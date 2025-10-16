use super::*;

pub(crate) struct Controller {
    client: Client,
    pool_difficulty: Arc<Mutex<Difficulty>>,
    extranonce1: Extranonce,
    extranonce2_size: u32,
    share_rx: mpsc::Receiver<(JobId, Header, Extranonce)>,
    share_tx: mpsc::Sender<(JobId, Header, Extranonce)>,
    cancel: CancellationToken,
    once: bool,
}

impl Controller {
    pub(crate) async fn new(mut client: Client, once: bool) -> Result<Self> {
        let (subscribe, _, _) = client.subscribe().await?;
        client.authorize().await?;

        info!(
            "Authorized: extranonce1={}, extranonce2_size={}",
            subscribe.extranonce1, subscribe.extranonce2_size
        );

        let (share_tx, share_rx) = mpsc::channel(32);

        Ok(Self {
            client,
            pool_difficulty: Arc::new(Mutex::new(Difficulty::default())),
            extranonce1: subscribe.extranonce1,
            extranonce2_size: subscribe.extranonce2_size,
            share_rx,
            share_tx,
            cancel: CancellationToken::new(),
            once,
        })
    }

    pub(crate) async fn run(mut self) -> Result {
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
                Some((job_id, header, extranonce2)) = self.share_rx.recv() => {
                    info!("Valid header found: {:?}", header);

                    if let Err(e) = self.client.submit(job_id, extranonce2, header.time.into(), header.nonce.into()).await {
                        warn!("Failed to submit share: {e}");
                    }

                    if self.once {
                        info!("Share found, exiting");
                        break;
                    }
                }
                _ = ctrl_c() => {
                    info!("Shutting down client and hasher");
                    break;
                }
            }
        }

        self.client.disconnect().await?;
        self.cancel_hasher();

        Ok(())
    }

    async fn handle_notification(&mut self, method: String, params: Value) -> Result {
        match method.as_str() {
            "mining.notify" => {
                let notify = serde_json::from_value::<Notify>(params)?;

                let extranonce2 = Extranonce::generate(self.extranonce2_size.try_into().unwrap());

                let share_tx = self.share_tx.clone();

                self.cancel_hasher();
                let cancel = self.cancel.clone();

                let network_nbits: CompactTarget = notify.nbits.into();
                let network_target: Target = network_nbits.into();
                let pool_target = self.pool_difficulty.lock().await.to_target();

                info!("{}", serde_json::to_string(&notify.merkle_branches)?);
                info!("Network target:\t{}", target_as_block_hash(network_target));
                info!("Pool target:\t{}", target_as_block_hash(pool_target));
                info!("Spawning hasher thread");

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
                    extranonce2,
                    job_id: notify.job_id,
                };

                // CPU-heavy task so spawning in it's own thread pool
                tokio::spawn(async move {
                    let result = tokio::task::spawn_blocking(move || hasher.hash(cancel)).await;

                    if let Ok(Ok(share)) = result {
                        if let Err(e) = share_tx.send(share).await {
                            error!("Failed to send found share: {e:#}");
                        }
                    } else if let Err(e) = result {
                        error!("Join error on hasher: {e:#}");
                    }
                });
            }
            "mining.set_difficulty" => {
                let difficulty = serde_json::from_value::<SetDifficulty>(params)?.difficulty();
                *self.pool_difficulty.lock().await = difficulty;
                info!("New difficulty: {difficulty}");
            }

            _ => warn!("Unhandled notification: {}", method),
        }

        Ok(())
    }

    async fn handle_request(&self, id: Id, method: String, params: Value) -> Result {
        info!("Got request method={method} with id={id} with params={params}");

        Ok(())
    }

    fn cancel_hasher(&mut self) {
        self.cancel.cancel();
        self.cancel = CancellationToken::new();
    }
}
