use {
    super::*,
    crate::{api, http_server, stratifier::SessionStore},
    pool_config::PoolConfig,
};

pub(crate) mod pool_config;

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) config: PoolConfig,
}

impl Pool {
    pub(crate) async fn run(&self, cancel_token: CancellationToken) -> Result {
        let config = Arc::new(self.config.clone());
        let session_store = Arc::new(SessionStore::new(SESSION_TTL));
        let metatron = Arc::new(Metatron::new());
        let (share_tx, share_rx) = mpsc::channel(SHARE_CHANNEL_CAPACITY);
        let connection_counter = AtomicU64::new(0);
        let address = config.address();
        let port = config.port();

        let mut generator =
            Generator::new(config.clone()).context("failed to connect to Bitcoin Core RPC")?;

        let workbase_receiver = generator
            .spawn()
            .await
            .context("failed to subscribe to ZMQ block notifications")?;

        let listener = TcpListener::bind((address.clone(), port))
            .await
            .with_context(|| format!("failed to bind to {address}:{port}"))?;

        info!("Listening on {address}:{port}");

        let metatron_handle = {
            let metatron = metatron.clone();
            let cancel = cancel_token.clone();
            tokio::spawn(async move {
                metatron.run(share_rx, None, cancel).await;
            })
        };

        let api_handle = if let Some(api_port) = config.api_port() {
            let http_config = http_server::HttpConfig {
                address: config.address(),
                port: api_port,
                acme_domains: config.acme_domains(),
                acme_contacts: config.acme_contacts(),
                acme_cache: config.acme_cache(),
            };

            Some(http_server::spawn(
                http_config,
                api::router(metatron.clone()),
                cancel_token.clone(),
            )?)
        } else {
            None
        };

        if !integration_test() && !logs_enabled() {
            spawn_throbber(metatron.clone());
        }

        let mut connection_tasks = JoinSet::new();

        loop {
            tokio::select! {
                Ok((stream, worker)) = listener.accept() => {
                    stream.set_nodelay(true)?;

                    let connection_id = connection_counter.fetch_add(1, Ordering::Relaxed);
                    info!("Accepted connection {} from {}", connection_id, worker);

                    let (reader, writer) = stream.into_split();

                    let workbase_receiver = workbase_receiver.clone();
                    let config = config.clone();
                    let metatron = metatron.clone();
                    let share_tx = share_tx.clone();
                    let session_store = session_store.clone();
                    let conn_cancel_token = cancel_token.child_token();

                    connection_tasks.spawn(async move {
                        let mut stratifier = Stratifier::new(
                            config,
                            metatron,
                            share_tx,
                            worker,
                            reader,
                            writer,
                            workbase_receiver,
                            conn_cancel_token,
                            session_store,
                        );

                        if let Err(err) = stratifier.serve().await {
                            error!("Stratifier {} error: {err}", connection_id)
                        }
                    });
                }
                _ = cancel_token.cancelled() => {
                        info!("Shutting down stratum server");
                        generator.shutdown().await;
                        break;
                    }
            }
        }

        info!(
            "Waiting for {} active connections to close...",
            connection_tasks.len()
        );

        while connection_tasks.join_next().await.is_some() {}

        info!("All connections closed");

        drop(share_tx);

        let _ = metatron_handle.await;
        info!("Metatron stopped");

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

    fn parse_pool_config(args: &str) -> PoolConfig {
        match Arguments::try_parse_from(args.split_whitespace()) {
            Ok(arguments) => match arguments.subcommand {
                Subcommand::Pool(pool) => pool.config,
                subcommand => panic!("unexpected subcommand: {subcommand:?}"),
            },
            Err(err) => panic!("error parsing arguments: {err}"),
        }
    }

    #[test]
    fn defaults_are_sane() {
        let config = parse_pool_config("para pool");

        assert_eq!(config.address(), "0.0.0.0");
        assert_eq!(config.port(), 42069);
        assert_eq!(config.chain(), Chain::Mainnet);
        assert_eq!(config.bitcoin_rpc_port(), config.chain().default_rpc_port());
        assert_eq!(
            config.bitcoin_rpc_url(),
            format!("127.0.0.1:{}/", config.bitcoin_rpc_port())
        );
        assert_eq!(
            config.version_mask(),
            Version::from_str("1fffe000").unwrap()
        );
        assert_eq!(config.update_interval(), Duration::from_secs(10));
        assert_eq!(
            config.zmq_block_notifications().to_string(),
            "tcp://127.0.0.1:28332".to_string()
        );
        assert_eq!(config.extranonce2_size(), 8);
    }

    #[test]
    fn override_address_and_port() {
        let config = parse_pool_config("para pool --address 127.0.0.1 --port 9999");

        assert_eq!(config.address(), "127.0.0.1");
        assert_eq!(config.port(), 9999);
    }

    #[test]
    fn override_chain_changes_default_rpc_port() {
        let config = parse_pool_config("para pool --chain signet");
        assert_eq!(config.chain(), Chain::Signet);
        assert_eq!(config.bitcoin_rpc_port(), config.chain().default_rpc_port());
    }

    #[test]
    fn explicit_bitcoin_rpc_port_wins() {
        let config = parse_pool_config("para pool --chain regtest --bitcoin-rpc-port 4242");
        assert_eq!(config.chain(), Chain::Regtest);
        assert_eq!(config.bitcoin_rpc_port(), 4242);
        assert_eq!(config.bitcoin_rpc_url(), "127.0.0.1:4242/");
    }

    #[test]
    fn override_version_mask() {
        let config = parse_pool_config("para pool --version-mask 00fff000");
        assert_eq!(
            config.version_mask(),
            Version::from_str("00fff000").unwrap()
        );
    }

    #[test]
    fn credentials_userpass_when_both_provided() {
        let config = parse_pool_config(
            "para pool \
                --bitcoin-rpc-username alice --bitcoin-rpc-password secret \
                --bitcoin-rpc-cookie-file /dev/null/.cookie",
        );

        match config.bitcoin_credentials().unwrap() {
            Auth::UserPass(username, password) => {
                assert_eq!(username, "alice");
                assert_eq!(password, "secret");
            }
            other => panic!("expected UserPass, got {other:?}"),
        }
    }

    #[test]
    fn credentials_fallback_to_cookie_when_partial_creds() {
        let config = parse_pool_config(
            "para pool \
                --bitcoin-rpc-username onlyuser \
                --bitcoin-rpc-cookie-file /tmp/test.cookie",
        );

        match config.bitcoin_credentials().unwrap() {
            Auth::CookieFile(path) => assert_eq!(path, PathBuf::from("/tmp/test.cookie")),
            other => panic!("expected CookieFile, got {other:?}"),
        }
    }

    #[test]
    fn credentials_cookiefile_when_no_creds() {
        let config =
            parse_pool_config("para pool --bitcoin-rpc-cookie-file /var/lib/bitcoind/.cookie");

        match config.bitcoin_credentials().unwrap() {
            Auth::CookieFile(path) => assert_eq!(path, PathBuf::from("/var/lib/bitcoind/.cookie")),
            other => panic!("expected CookieFile, got {other:?}"),
        }
    }

    #[test]
    fn cookie_file_from_explicit_cookie_path() {
        let config = parse_pool_config("para pool --bitcoin-rpc-cookie-file /x/y/.cookie");
        assert_eq!(config.cookie_file().unwrap(), PathBuf::from("/x/y/.cookie"));
    }

    #[test]
    fn cookie_file_from_bitcoin_data_dir_and_chain() {
        let config =
            parse_pool_config("para pool --bitcoin-data-dir /data/bitcoin --chain regtest");

        assert_eq!(
            config.cookie_file().unwrap(),
            PathBuf::from("/data/bitcoin/regtest/.cookie")
        );

        let config = parse_pool_config("para pool --bitcoin-data-dir /data/bitcoin --chain signet");
        assert_eq!(
            config.cookie_file().unwrap(),
            PathBuf::from("/data/bitcoin/signet/.cookie")
        );

        let config =
            parse_pool_config("para pool --bitcoin-data-dir /data/bitcoin --chain mainnet");

        assert_eq!(
            config.cookie_file().unwrap(),
            PathBuf::from("/data/bitcoin/.cookie")
        );
    }

    #[test]
    fn rpc_url_reflects_port_choice() {
        let config = parse_pool_config("para pool --bitcoin-rpc-port 12345");
        assert_eq!(config.bitcoin_rpc_url(), "127.0.0.1:12345/");
    }

    #[test]
    fn zmq_block_notifications() {
        let config = parse_pool_config("para pool --zmq-block-notifications tcp://127.0.0.1:69");
        assert_eq!(
            config.zmq_block_notifications(),
            "tcp://127.0.0.1:69".parse().unwrap()
        );
    }

    #[test]
    fn start_diff() {
        let config = parse_pool_config("para pool --start-diff 0.00001");
        assert_eq!(config.start_diff(), Difficulty::from(0.00001));

        let config = parse_pool_config("para pool --start-diff 111");
        assert_eq!(config.start_diff(), Difficulty::from(111));

        let config = parse_pool_config("para pool");
        assert_eq!(config.start_diff(), Difficulty::from(1));
    }

    #[test]
    fn vardiff_period() {
        let config = parse_pool_config("para pool --vardiff-period 10.0");
        assert_eq!(config.vardiff_period(), Duration::from_secs(10));

        let config = parse_pool_config("para pool --vardiff-period 0.5");
        assert_eq!(config.vardiff_period(), Duration::from_millis(500));

        let config = parse_pool_config("para pool");
        assert_eq!(config.vardiff_period(), Duration::from_secs(5));
    }

    #[test]
    fn vardiff_window() {
        let config = parse_pool_config("para pool --vardiff-window 60");
        assert_eq!(config.vardiff_window(), Duration::from_secs(60));

        let config = parse_pool_config("para pool --vardiff-window 600.5");
        assert_eq!(config.vardiff_window(), Duration::from_secs_f64(600.5));

        let config = parse_pool_config("para pool");
        assert_eq!(config.vardiff_window(), Duration::from_secs(300));
    }

    #[test]
    fn extranonce2_size_default() {
        let config = parse_pool_config("para pool");
        assert_eq!(config.extranonce2_size(), 8);
    }

    #[test]
    fn extranonce2_size_override() {
        let config = parse_pool_config("para pool --extranonce2-size 4");
        assert_eq!(config.extranonce2_size(), 4);
    }

    #[test]
    fn extranonce2_size_boundaries() {
        let config = parse_pool_config("para pool --extranonce2-size 2");
        assert_eq!(config.extranonce2_size(), 2);

        let config = parse_pool_config("para pool --extranonce2-size 8");
        assert_eq!(config.extranonce2_size(), 8);
    }

    #[test]
    #[should_panic(expected = "error parsing arguments")]
    fn extranonce2_size_too_small() {
        parse_pool_config("para pool --extranonce2-size 1");
    }

    #[test]
    #[should_panic(expected = "error parsing arguments")]
    fn extranonce2_size_too_large() {
        parse_pool_config("para pool --extranonce2-size 9");
    }
}
