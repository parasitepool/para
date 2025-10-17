use {super::*, pool_config::PoolConfig};

pub(crate) mod pool_config;

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) config: PoolConfig,
}

impl Pool {
    pub(crate) async fn run(&self) -> Result {
        let config = Arc::new(self.config.clone());
        let address = config.address();
        let port = config.port();

        let mut generator = Generator::new(config.clone())?;
        let template_receiver = generator.spawn().await?;

        let listener = TcpListener::bind((address.clone(), port)).await?;

        eprintln!("Listening on {address}:{port}");

        loop {
            tokio::select! {
                Ok((stream, worker)) = listener.accept() => {
                    stream.set_nodelay(true)?;

                    info!("Accepted connection from {worker}");

                    let (reader, writer) = stream.into_split();

                    let template_receiver = template_receiver.clone();
                    let config = config.clone();

                    tokio::task::spawn(async move {
                        let mut conn = Connection::new(config, worker, reader, writer, template_receiver);

                        if let Err(err) = conn.serve().await {
                            error!("Worker connection error: {err}")
                        }
                    });
                }
                _ = ctrl_c() => {
                        info!("Shutting down stratum server");
                        generator.shutdown().await;
                        break;
                    }
            }
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
}
