use {super::*, settings::Settings, subcommand::pool::pool_config::PoolConfig};

pub(crate) struct Generator {
    bitcoin_rpc_client: Arc<bitcoincore_rpc::Client>,
    cancel: CancellationToken,
    settings: Arc<Settings>,
    config: Arc<PoolConfig>,
    handle: Option<JoinHandle<()>>,
}

impl Generator {
    pub(crate) fn new(settings: Arc<Settings>, config: Arc<PoolConfig>) -> Result<Self> {
        Ok(Self {
            bitcoin_rpc_client: Arc::new(settings.bitcoin_rpc_client()?),
            cancel: CancellationToken::new(),
            settings,
            config,
            handle: None,
        })
    }

    pub(crate) async fn spawn(&mut self) -> Result<watch::Receiver<Arc<Workbase>>> {
        let rpc = self.bitcoin_rpc_client.clone();
        let cancel = self.cancel.clone();
        let settings = self.settings.clone();
        let config = self.config.clone();

        let initial = get_block_template_blocking(&rpc, &settings)?;
        let (tx, rx) = watch::channel(Arc::new(Workbase::new(initial)));

        let mut subscription = Zmq::connect(&settings, &config).await?;

        let handle = tokio::spawn({
            info!("Spawning generator task");

            let update_interval = config.update_interval(&settings);
            let mut ticker = interval(update_interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

            let fetch_and_push = move || {
                let rpc = rpc.clone();
                let settings = settings.clone();
                let tx = tx.clone();
                task::spawn_blocking(move || match get_block_template_blocking(&rpc, &settings) {
                    Ok(template) => {
                        tx.send_replace(Arc::new(Workbase::new(template)));
                    }
                    Err(err) => warn!("Failed to fetch new block template: {err}"),
                });
            };

            async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        result = subscription.recv_blockhash() => {
                            match result {
                                Ok(blockhash) => {
                                    info!("ZMQ blockhash {blockhash}");
                                    fetch_and_push();
                                }
                                Err(err) => error!("ZMQ receive error: {err}"),
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

        self.handle = Some(handle);
        Ok(rx)
    }

    pub(crate) async fn shutdown(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

fn get_block_template_blocking(
    bitcoin_rpc_client: &bitcoincore_rpc::Client,
    settings: &Settings,
) -> Result<BlockTemplate> {
    let mut rules = vec!["segwit"];
    if settings.chain().network() == Network::Signet {
        rules.push("signet");
    }

    let params = json!({
        "capabilities": ["coinbasetxn", "workid", "coinbase/append"],
        "rules": rules,
    });

    let template = bitcoin_rpc_client.call::<BlockTemplate>("getblocktemplate", &[params])?;

    info!("New block template for height {}", template.height);

    Ok(template)
}
