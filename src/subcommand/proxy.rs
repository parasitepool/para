use {
    super::*,
    crate::{api, event_sink::build_event_sink, http_server},
};

#[derive(Parser, Debug)]
pub(crate) struct Proxy {
    #[command(flatten)]
    pub(crate) options: ProxyOptions,
}

impl Proxy {
    pub(crate) async fn run(
        &self,
        cancel_token: CancellationToken,
        logs: Arc<logs::Logs>,
    ) -> Result {
        let tasks = TaskTracker::new();

        let settings = Arc::new(
            Settings::from_proxy_options(self.options.clone())
                .context("failed to create settings")?,
        );

        let bitcoin_client = Arc::new(settings.bitcoin_rpc_client().await?);

        let address = settings.address();
        let port = settings.port();
        let listener = TcpListener::bind((address, port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        info!("Stratum proxy listening for downstream miners on {address}:{port}");

        let mut backoff = Duration::from_secs(1);
        let timeout = settings.timeout();
        let upstream_target = settings
            .upstream_targets()
            .first()
            .context("no upstream target configured")?;

        let metatron = Arc::new(Metatron::new());

        let upstream_id = metatron.next_upstream_id();

        let Some(mut upstream) = connect_upstream(
            upstream_id,
            upstream_target,
            timeout,
            &cancel_token,
            &tasks,
            &mut backoff,
        )
        .await
        else {
            return Ok(());
        };

        let extranonces = Extranonces::Proxy(
            ProxyExtranonces::new(
                upstream.enonce1().clone(),
                upstream.enonce2_size(),
                settings.enonce1_extension_size(),
            )
            .context("upstream extranonce configuration incompatible with proxy mode")?,
        );

        let allocator = Arc::new(EnonceAllocator::new(extranonces, upstream_id));

        metatron.clone().spawn(cancel_token.clone(), &tasks);

        let metrics = Arc::new(Metrics::new(upstream.clone(), metatron.clone()));

        http_server::spawn(
            &settings,
            api::proxy::router(metrics.clone(), bitcoin_client, settings.chain(), logs),
            cancel_token.clone(),
            &tasks,
        )?;

        let event_tx = build_event_sink(&settings, cancel_token.clone(), &tasks)
            .await
            .context("failed to build record sink")?;

        if !integration_test() && !logs_enabled() {
            spawn_throbber(metrics.clone(), cancel_token.clone(), &tasks);
        }

        loop {
            loop {
                let (stream, addr) = tokio::select! {
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, addr)) => (stream, addr),
                            Err(err) => {
                                error!("Accept error: {err}");
                                continue;
                            }
                        }
                    }
                    _ = upstream.disconnected() => {
                        warn!("Upstream connection lost, reconnecting...");
                        break;
                    }
                    _ = cancel_token.cancelled() => {
                        info!("Shutting down proxy");
                        tasks.close();
                        tasks.wait().await;
                        info!("All proxy tasks stopped");
                        return Ok(());
                    }
                };

                debug!("Spawning stratifier task for {addr}");

                let settings = settings.clone();
                let allocator = allocator.clone();
                let metatron = metatron.clone();
                let upstream = upstream.clone();
                let conn_cancel_token = cancel_token.child_token();
                let event_tx = event_tx.clone();
                let start_diff = settings.start_diff();

                tasks.spawn(async move {
                    let workbase_rx = upstream.workbase_rx();
                    let mut stratifier: Stratifier<Notify> = Stratifier::new(
                        addr,
                        settings,
                        allocator,
                        metatron,
                        Some(upstream),
                        stream,
                        workbase_rx,
                        conn_cancel_token,
                        event_tx,
                        start_diff,
                    );

                    if let Err(err) = stratifier.serve().await {
                        error!("Stratifier error for {addr}: {err}");
                    }
                });
            }

            let new_id = metatron.next_upstream_id();

            let Some(new_upstream) = connect_upstream(
                new_id,
                upstream_target,
                timeout,
                &cancel_token,
                &tasks,
                &mut backoff,
            )
            .await
            else {
                break;
            };

            let new_extranonces = Extranonces::Proxy(
                ProxyExtranonces::new(
                    new_upstream.enonce1().clone(),
                    new_upstream.enonce2_size(),
                    settings.enonce1_extension_size(),
                )
                .context("upstream extranonce configuration incompatible with proxy mode")?,
            );

            allocator.update_extranonces(new_extranonces);
            allocator.set_upstream_id(new_id);
            metrics.update_upstream(new_upstream.clone());
            upstream = new_upstream;
        }

        tasks.close();
        tasks.wait().await;

        info!("All proxy tasks stopped");

        Ok(())
    }
}

async fn connect_upstream(
    upstream_id: u32,
    target: &UpstreamTarget,
    timeout: Duration,
    cancel_token: &CancellationToken,
    tasks: &TaskTracker,
    backoff: &mut Duration,
) -> Option<Arc<Upstream>> {
    let mut max_backoff_attempts = 0;

    loop {
        match Upstream::connect(upstream_id, target, timeout, cancel_token.clone(), tasks).await {
            Ok(upstream) => {
                *backoff = Duration::from_secs(1);
                return Some(upstream);
            }
            Err(e) => {
                warn!("Failed to connect to upstream: {e}");
            }
        }

        warn!("Retrying in {}s...", backoff.as_secs());

        tokio::select! {
            _ = sleep(*backoff) => {}
            _ = cancel_token.cancelled() => return None
        }

        *backoff = (*backoff * 2).min(Duration::from_secs(60));

        if *backoff >= Duration::from_secs(60) {
            max_backoff_attempts += 1;
            if max_backoff_attempts >= 3 {
                error!(
                    "Upstream unreachable after {max_backoff_attempts} attempts at max backoff, exiting"
                );
                return None;
            }
        }
    }
}
