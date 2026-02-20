use super::*;

pub(crate) async fn spawn_generator(
    rpc: Arc<BitcoindClient>,
    settings: Arc<Settings>,
    cancel: CancellationToken,
    tasks: &mut JoinSet<()>,
) -> Result<watch::Receiver<Arc<BlockTemplate>>> {
    info!("Spawning generator task");

    let initial = get_block_template(&rpc, &settings).await?;
    let (tx, rx) = watch::channel(Arc::new(initial));

    let mut subscription = Zmq::connect(settings.clone()).await?;

    let mut ticker = interval(settings.update_interval());
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    tasks.spawn(async move {
        let fetch_and_push = || async {
            match get_block_template(&rpc, &settings).await {
                Ok(template) => {
                    tx.send_replace(Arc::new(template));
                }
                Err(err) => warn!("Failed to fetch new block template: {err}"),
            }
        };

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                result = subscription.recv_blockhash() => {
                    match result {
                        Ok(blockhash) => {
                            info!("ZMQ blockhash {blockhash}");
                            fetch_and_push().await;
                        }
                        Err(err) => error!("ZMQ receive error: {err}"),
                    }
                }
                _ = ticker.tick() => fetch_and_push().await,
            }
        }
        info!("Shutting down generator");
    });

    Ok(rx)
}

async fn get_block_template(
    bitcoin_rpc_client: &BitcoindClient,
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

    let gbt: block_template::GetBlockTemplate = bitcoin_rpc_client
        .call_raw("getblocktemplate", &[params])
        .await?;

    let block_template = BlockTemplate::from(gbt);

    info!("New block template for height {}", block_template.height);

    Ok(block_template)
}
