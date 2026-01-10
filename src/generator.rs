use super::*;

pub(crate) async fn spawn_generator(
    settings: Arc<Settings>,
    cancel: CancellationToken,
    tasks: &mut JoinSet<()>,
) -> Result<watch::Receiver<Arc<BlockTemplate>>> {
    info!("Spawning generator task");
    let rpc = Arc::new(settings.bitcoin_rpc_client()?);

    let initial = get_block_template_blocking(&rpc, &settings)?;
    let (tx, rx) = watch::channel(Arc::new(initial));

    let mut subscription = Zmq::connect(settings.clone()).await?;

    let mut ticker = interval(settings.update_interval());
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    tasks.spawn(async move {
        let fetch_and_push = || {
            let rpc = rpc.clone();
            let settings = settings.clone();
            let tx = tx.clone();
            task::spawn_blocking(move || match get_block_template_blocking(&rpc, &settings) {
                Ok(template) => {
                    tx.send_replace(Arc::new(template));
                }
                Err(err) => warn!("Failed to fetch new block template: {err}"),
            });
        };

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
    });

    Ok(rx)
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

    let gbt = bitcoin_rpc_client
        .call::<block_template::GetBlockTemplate>("getblocktemplate", &[params])?;

    let block_template = BlockTemplate::from(gbt);

    info!("New block template for height {}", block_template.height);

    Ok(block_template)
}
