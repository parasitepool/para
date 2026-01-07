use {
    super::*,
    crate::http_server,
    proxy_config::ProxyConfig,
    stratum::{Client, ClientConfig},
};

pub(crate) mod proxy_config;

#[derive(Parser, Debug)]
pub(crate) struct Proxy {
    #[command(flatten)]
    pub(crate) config: ProxyConfig,
}

impl Proxy {
    pub(crate) async fn run(&self, cancel_token: CancellationToken) -> Result {
        let config = Arc::new(self.config.clone());

        let upstream_addr = resolve_stratum_endpoint(config.upstream())
            .await
            .with_context(|| {
                format!(
                    "failed to resolve upstream endpoint `{}`",
                    config.upstream()
                )
            })?;

        info!(
            "Connecting to upstream {} ({}) as {}",
            config.upstream(),
            upstream_addr,
            config.username()
        );

        let client_config = ClientConfig {
            address: upstream_addr.to_string(),
            username: config.username(),
            user_agent: USER_AGENT.into(),
            password: config.password(),
            timeout: config.timeout(),
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

        info!("Authorized with upstream as {}", config.username());

        let proxy_status = Arc::new(api::proxy::ProxyStatus::new(
            config.upstream().to_string(),
            config.username().to_string(),
            config.address(),
            config.port(),
        ));

        proxy_status.set_connected(true);

        let api_handle = if let Some(api_port) = config.api_port() {
            let http_config = http_server::HttpConfig {
                address: config.address(),
                port: api_port,
                acme_domains: vec![],
                acme_contacts: vec![],
                acme_cache: PathBuf::new(),
            };

            info!("Starting HTTP API on {}:{}", config.address(), api_port);

            Some(http_server::spawn(
                http_config,
                api::proxy::router(proxy_status.clone()),
                cancel_token.clone(),
            )?)
        } else {
            None
        };

        info!(
            "Proxy ready. Listening for downstream miners on {}:{}",
            config.address(),
            config.port()
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
                            proxy_status.set_connected(false);
                            break;
                        }
                        Err(e) => {
                            error!("Upstream event error: {}", e);
                            proxy_status.set_connected(false);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arguments::Arguments;

    fn parse_proxy_config(args: &str) -> ProxyConfig {
        match Arguments::try_parse_from(args.split_whitespace()) {
            Ok(arguments) => match arguments.subcommand {
                Subcommand::Proxy(proxy) => proxy.config,
                subcommand => panic!("unexpected subcommand: {subcommand:?}"),
            },
            Err(err) => panic!("error parsing arguments: {err}"),
        }
    }

    #[test]
    fn defaults_are_sane() {
        let config = parse_proxy_config("para proxy pool.example.com:3333 --username bc1qtest");

        assert_eq!(config.upstream(), "pool.example.com:3333");
        assert_eq!(config.username().to_string(), "bc1qtest");
        assert_eq!(config.password(), None);
        assert_eq!(config.address(), "0.0.0.0");
        assert_eq!(config.port(), 42069);
        assert_eq!(config.api_port(), None);
        assert_eq!(config.timeout(), Duration::from_secs(30));
    }

    #[test]
    fn override_address_and_port() {
        let config = parse_proxy_config(
            "para proxy pool.example.com:3333 --username bc1qtest --address 127.0.0.1 --port 9999",
        );

        assert_eq!(config.address(), "127.0.0.1");
        assert_eq!(config.port(), 9999);
    }

    #[test]
    fn override_api_port() {
        let config = parse_proxy_config(
            "para proxy pool.example.com:3333 --username bc1qtest --api-port 8080",
        );

        assert_eq!(config.api_port(), Some(8080));
    }

    #[test]
    fn override_timeout() {
        let config =
            parse_proxy_config("para proxy pool.example.com:3333 --username bc1qtest --timeout 60");

        assert_eq!(config.timeout(), Duration::from_secs(60));
    }

    #[test]
    fn password_override() {
        let config = parse_proxy_config(
            "para proxy pool.example.com:3333 --username bc1qtest --password secret",
        );

        assert_eq!(config.password(), Some("secret".to_string()));
    }

    #[test]
    fn username_with_worker() {
        let config =
            parse_proxy_config("para proxy pool.example.com:3333 --username bc1qtest.worker1");

        assert_eq!(config.username().to_string(), "bc1qtest.worker1");
    }
}
