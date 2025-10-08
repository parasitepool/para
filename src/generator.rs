use super::*;

// - get's new template every n seconds (configurable)
// - listens on zmq for new blocks, if so generators new template immediately
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

        self.join = Some(task::spawn_blocking(move || {
            while !cancel.is_cancelled() {
                for _ in 0..5 {
                    if cancel.is_cancelled() {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                match get_block_template_blocking(&bitcoin_rpc_client, &config) {
                    Ok(template) => {
                        template_sender.send_replace(Arc::new(template));
                    }
                    Err(err) => {
                        warn!("Failed to fetch block template: {err}");
                    }
                }
            }
            info!("Shutting down generator")
        }));

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
