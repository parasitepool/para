use super::*;

pub(crate) struct Generator {
    bitcoin_rpc_client: Arc<bitcoincore_rpc::Client>,
    cancel: CancellationToken,
    config: Arc<PoolConfig>,
    join: Option<JoinHandle<()>>,
}

impl Generator {
    pub(crate) fn new(config: Arc<PoolConfig>) -> Result<Self> {
        Ok(Self {
            bitcoin_rpc_client: Arc::new(config.bitcoin_rpc_client()?),
            cancel: CancellationToken::new(),
            config: config.clone(),
            join: None,
        })
    }

    pub(crate) fn spawn(&mut self) -> Result<watch::Receiver<Arc<BlockTemplate>>> {
        let bitcoin_rpc_client = self.bitcoin_rpc_client.clone();
        let cancel = self.cancel.clone();
        let config = self.config.clone();

        let initial_template = get_block_template_blocking(&bitcoin_rpc_client, &config)?;

        let (template_sender, template_receiver) = watch::channel(Arc::new(initial_template));

        let join = tokio::spawn({
            info!("Spawning generator");

            let mut ticker = interval(config.update_interval());
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

            async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = ticker.tick() => {
                            let bitcoin_rpc_client = bitcoin_rpc_client.clone();
                            let config = config.clone();
                            let template_sender = template_sender.clone();
                            task::spawn_blocking(move ||  {
                                match get_block_template_blocking(&bitcoin_rpc_client, &config) {
                                    Ok(template) => {
                                        template_sender.send_replace(Arc::new(template));
                                    },
                                    Err(err) => {
                                        warn!("Failed to fetch new block template: {err}");
                                    },
                                }
                            });
                        }
                    }
                }
                info!("Shutting down generator");
            }
        });

        self.join = Some(join);

        Ok(template_receiver)
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
