use super::*;

pub(crate) async fn spawn_generator(
    rpc: Arc<BitcoindClient>,
    settings: Arc<Settings>,
    cancel: CancellationToken,
    tasks: &TaskTracker,
) -> Result<watch::Receiver<Arc<BlockTemplate>>> {
    info!("Spawning generator task");

    let initial = get_block_template(&rpc, &settings).await?;
    let (tx, rx) = watch::channel(Arc::new(initial));

    let mut subscription = Zmq::connect(settings.clone()).await?;

    let mut ticker = interval(settings.update_interval());
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let bitcoind_timeout = settings.bitcoind_timeout();

    tasks.spawn(async move {
        let fetch = || async {
            let template = get_block_template(&rpc, &settings).await?;
            tx.send_replace(Arc::new(template));
            Ok::<_, Error>(())
        };

        let mut rpc_fail_since: Option<Instant> = None;
        let mut zmq_fail_since: Option<Instant> = None;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                result = subscription.recv_blockhash() => {
                    match result {
                        Ok(blockhash) => {
                            zmq_fail_since = None;
                            info!("ZMQ blockhash {blockhash}");
                        }
                        Err(err) => {
                            error!("ZMQ receive error: {err}");
                            if !zmq_reconnect(
                                &mut subscription,
                                &mut zmq_fail_since,
                                bitcoind_timeout,
                                &settings,
                                &cancel,
                            )
                            .await
                            {
                                break;
                            }
                            continue;
                        }
                    }
                }
                _ = ticker.tick() => {}
            }

            match fetch().await {
                Ok(()) => rpc_fail_since = None,
                Err(err) => {
                    warn!("Failed to fetch new block template: {err}");
                    if timed_out(&mut rpc_fail_since, bitcoind_timeout) {
                        error!(
                            "bitcoind RPC unavailable for over {bitcoind_timeout:?}, shutting down"
                        );
                        cancel.cancel();
                        break;
                    }
                }
            }
        }
        info!("Shutting down generator");
    });

    Ok(rx)
}

async fn zmq_reconnect(
    subscription: &mut Zmq,
    zmq_fail_since: &mut Option<Instant>,
    bitcoind_timeout: Duration,
    settings: &Arc<Settings>,
    cancel: &CancellationToken,
) -> bool {
    let _ = zmq_fail_since.get_or_insert_with(Instant::now);
    let mut backoff = Duration::from_secs(1);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return false,
            _ = sleep(backoff) => {}
        }
        match Zmq::connect(settings.clone()).await {
            Ok(new_sub) => {
                info!("ZMQ reconnected");
                *subscription = new_sub;
                *zmq_fail_since = None;
                return true;
            }
            Err(err) => {
                warn!("ZMQ reconnection failed: {err}");
                if timed_out(zmq_fail_since, bitcoind_timeout) {
                    error!("bitcoind ZMQ unavailable for over {bitcoind_timeout:?}, shutting down");
                    cancel.cancel();
                    return false;
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

fn timed_out(fail_since: &mut Option<Instant>, timeout: Duration) -> bool {
    fail_since.get_or_insert_with(Instant::now).elapsed() > timeout
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
