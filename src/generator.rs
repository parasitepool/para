use super::*;

pub(crate) async fn spawn_generator(
    rpc: Arc<BitcoindClient>,
    settings: Arc<Settings>,
    cancel: CancellationToken,
    tasks: &TaskTracker,
) -> Result<watch::Receiver<Arc<BlockTemplate>>> {
    info!("Spawning generator task");

    verify_zmq_hashblock(&rpc, &settings).await?;

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
    let fail_start = *zmq_fail_since.get_or_insert_with(Instant::now);
    let mut backoff = Duration::from_secs(1);
    loop {
        let remaining = bitcoind_timeout.saturating_sub(fail_start.elapsed());
        if remaining.is_zero() {
            error!("bitcoind ZMQ unavailable for over {bitcoind_timeout:?}, shutting down");
            cancel.cancel();
            return false;
        }
        tokio::select! {
            _ = cancel.cancelled() => return false,
            _ = sleep(backoff.min(remaining)) => {}
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
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

fn timed_out(fail_since: &mut Option<Instant>, timeout: Duration) -> bool {
    fail_since.get_or_insert_with(Instant::now).elapsed() > timeout
}

async fn verify_zmq_hashblock(rpc: &BitcoindClient, settings: &Settings) -> Result<()> {
    #[derive(Debug, Deserialize)]
    struct ZmqNotification {
        #[serde(rename = "type")]
        notification_type: String,
        address: String,
    }

    let notifications: Vec<ZmqNotification> = rpc
        .call_raw("getzmqnotifications", &[])
        .await
        .context("failed to call getzmqnotifications")?;

    let expected = settings.zmq_block_notifications().to_string();

    let has_hashblock = notifications
        .iter()
        .any(|n| n.notification_type == "pubhashblock" && n.address == expected);

    ensure!(
        has_hashblock,
        "bitcoind is not publishing hashblock notifications on {expected} \
         - add `zmqpubhashblock={expected}` to bitcoin.conf"
    );

    Ok(())
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
