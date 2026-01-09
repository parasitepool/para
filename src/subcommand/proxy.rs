use {
    super::*,
    crate::{
        http_server,
        settings::{ProxyOptions, Settings},
    },
    stratum::{Client, ClientConfig},
};

#[derive(Parser, Debug)]
pub(crate) struct Proxy {
    #[command(flatten)]
    pub(crate) options: ProxyOptions,
}

impl Proxy {
    pub(crate) async fn run(&self, cancel_token: CancellationToken) -> Result {
        let settings = Arc::new(
            Settings::from_proxy_options(self.options.clone())
                .context("failed to create settings")?,
        );

        let upstream = settings.upstream().context("proxy configuration error")?;
        let username = settings
            .upstream_username()
            .context("proxy configuration error")?;

        let upstream_addr = resolve_stratum_endpoint(upstream)
            .await
            .with_context(|| format!("failed to resolve upstream endpoint `{upstream}`"))?;

        info!(
            "Connecting to upstream {} ({}) as {}",
            upstream, upstream_addr, username
        );

        let client_config = ClientConfig {
            address: upstream_addr.to_string(),
            username: username.clone(),
            user_agent: USER_AGENT.into(),
            password: settings.upstream_password().map(String::from),
            timeout: settings.timeout(),
        };

        let client = Client::new(client_config);

        let mut events = client
            .connect()
            .await
            .context("failed to connect to upstream")?;

        let (subscribe_result, _, _) = client
            .subscribe()
            .await
            .context("failed to subscribe to upstream")?;

        info!(
            "Subscribed to upstream: enonce1={}, enonce2_size={}",
            subscribe_result.enonce1, subscribe_result.enonce2_size
        );

        client
            .authorize()
            .await
            .context("failed to authorize with upstream")?;

        info!("Authorized with upstream as {}", username);

        let nexus = Arc::new(Nexus::new(
            upstream.to_string(),
            username.to_string(),
            settings.address().to_string(),
            settings.port(),
        ));

        nexus.set_connected(true);

        let api_handle = if let Some(api_port) = settings.api_port() {
            let http_config = http_server::HttpConfig {
                address: settings.address().to_string(),
                port: api_port,
                acme_domains: vec![],
                acme_contacts: vec![],
                acme_cache: PathBuf::new(),
            };

            info!("Starting HTTP API on {}:{}", settings.address(), api_port);

            Some(http_server::spawn(
                http_config,
                api::proxy::router(nexus.clone()),
                cancel_token.clone(),
            )?)
        } else {
            None
        };

        info!(
            "Proxy ready. Listening for downstream miners on {}:{}",
            settings.address(),
            settings.port()
        );

        loop {
            tokio::select! {
                event = events.recv() => {
                    match event {
                        Ok(stratum::Event::Notify(notify)) => {
                            debug!("Received notify: job_id={}, clean_jobs={}", notify.job_id, notify.clean_jobs);
                        }
                        Ok(stratum::Event::SetDifficulty(diff)) => {
                            debug!("Received set_difficulty: {}", diff);
                        }
                        Ok(stratum::Event::Disconnected) => {
                            warn!("Disconnected from upstream");
                            nexus.set_connected(false);
                            break;
                        }
                        Err(e) => {
                            error!("Upstream event error: {}", e);
                            nexus.set_connected(false);
                            break;
                        }
                    }
                }
                _ = cancel_token.cancelled() => {
                    info!("Shutting down proxy");
                    break;
                }
            }
        }

        client.disconnect().await;
        info!("Disconnected from upstream");

        if let Some(handle) = api_handle {
            let _ = handle.await;
            info!("HTTP API server stopped");
        }

        Ok(())
    }
}
