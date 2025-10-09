use super::*;

pub(crate) struct Generator {
    bitcoin_rpc: Arc<bitcoincore_rpc::Client>,
    cancel: CancellationToken,
    config: Arc<PoolConfig>,
    join: Option<JoinHandle<()>>,
}

impl Generator {
    pub(crate) fn new(config: Arc<PoolConfig>) -> Result<Self> {
        Ok(Self {
            bitcoin_rpc: Arc::new(config.bitcoin_rpc_client()?),
            cancel: CancellationToken::new(),
            config: config.clone(),
            join: None,
        })
    }

    pub(crate) async fn spawn(&mut self) -> Result<watch::Receiver<Arc<BlockTemplate>>> {
        let rpc = self.bitcoin_rpc.clone();
        let cancel = self.cancel.clone();
        let config = self.config.clone();

        let initial = get_block_template_blocking(&rpc, &config)?;
        let (tx, rx) = watch::channel(Arc::new(initial));

        let zmq_endpoint = config.zmq_block_notifications().to_string();
        info!("Subscribing to hashblock on ZMQ endpoint {zmq_endpoint}");

        let mut subscription = timeout(Duration::from_secs(1), async {
            let mut socket = SubSocket::new();
            socket.connect(&zmq_endpoint).await.context("ZMQ connect")?;
            socket
                .subscribe("hashblock")
                .await
                .context("ZMQ subscribe to `hashblock`")?;
            Ok::<_, Error>(socket)
        })
        .await
        .map_err(|err| anyhow!("Failed to connect to ZMQ endpoint {zmq_endpoint}: {err}"))??;

        let handle = tokio::spawn({
            info!("Spawning generator task");

            let mut ticker = interval(config.update_interval());
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

            let fetch_and_push = move || {
                let rpc = rpc.clone();
                let config = config.clone();
                let tx = tx.clone();
                task::spawn_blocking(move || match get_block_template_blocking(&rpc, &config) {
                    Ok(template) => {
                        tx.send_replace(Arc::new(template));
                    }
                    Err(err) => warn!("Failed to fetch new block template: {err}"),
                });
            };

            async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        result = subscription.recv() => {
                            match result {
                                Ok(message) => {
                                    let slice = message.get(1).unwrap().iter().rev().copied().collect::<Vec<_>>();
                                    let blockhash = BlockHash::from_slice(&slice).unwrap();
                                    info!("ZMQ blockhash: {blockhash}");
                                    fetch_and_push();
                                }
                                Err(err) => error!("Failed to get blockhash from ZMQ: {err}")
                            }
                        }
                        _ = ticker.tick() => {
                            fetch_and_push();
                        }
                    }
                }
                info!("Shutting down generator");
            }
        });

        self.join = Some(handle);
        Ok(rx)
    }

    pub(crate) async fn shutdown(&mut self) {
        self.cancel.cancel();
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

fn get_block_template_blocking(
    bitcoin_rpc_client: &bitcoincore_rpc::Client,
    config: &PoolConfig,
) -> Result<BlockTemplate> {
    info!("Fetching new block template");

    let mut rules = vec!["segwit"];
    if config.chain().network() == Network::Signet {
        rules.push("signet");
    }

    let params = json!({
        "capabilities": ["coinbasetxn", "workid", "coinbase/append"],
        "rules": rules,
    });

    Ok(bitcoin_rpc_client.call::<BlockTemplate>("getblocktemplate", &[params])?)
}
