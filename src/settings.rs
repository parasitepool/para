use super::*;

mod pool_options;
mod proxy_options;

pub(crate) use pool_options::PoolOptions;
pub(crate) use proxy_options::ProxyOptions;

#[derive(Clone, Debug)]
pub(crate) struct Settings {
    address: String,
    port: u16,
    api_port: Option<u16>,
    upstream_endpoint: Option<String>,
    upstream_username: Option<Username>,
    upstream_password: Option<String>,
    timeout: Duration,
    bitcoin_data_dir: Option<PathBuf>,
    bitcoin_rpc_password: Option<String>,
    bitcoin_rpc_port: u16,
    bitcoin_rpc_username: Option<String>,
    bitcoin_rpc_cookie_file: Option<PathBuf>,
    chain: Chain,
    acme_domains: Vec<String>,
    acme_contacts: Vec<String>,
    acme_cache: PathBuf,
    data_dir: Option<PathBuf>,
    update_interval: Duration,
    version_mask: Version,
    start_diff: Difficulty,
    min_diff: Option<Difficulty>,
    max_diff: Option<Difficulty>,
    vardiff_period: Duration,
    vardiff_window: Duration,
    zmq_block_notifications: Endpoint,
    extranonce2_size: u8,
    disable_bouncer: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            address: "0.0.0.0".into(),
            port: 42069,
            api_port: None,
            upstream_endpoint: None,
            upstream_username: None,
            upstream_password: None,
            timeout: Duration::from_secs(30),
            bitcoin_data_dir: None,
            bitcoin_rpc_password: None,
            bitcoin_rpc_port: Chain::Mainnet.default_rpc_port(),
            bitcoin_rpc_username: None,
            bitcoin_rpc_cookie_file: None,
            chain: Chain::Mainnet,
            acme_domains: vec![],
            acme_contacts: vec![],
            acme_cache: PathBuf::from("acme-cache"),
            data_dir: None,
            update_interval: Duration::from_secs(10),
            version_mask: "1fffe000".parse().unwrap(),
            start_diff: Difficulty::from(1.0),
            min_diff: None,
            max_diff: None,
            vardiff_period: Duration::from_secs(5),
            vardiff_window: Duration::from_secs(300),
            zmq_block_notifications: "tcp://127.0.0.1:28332".parse().unwrap(),
            extranonce2_size: 8,
            disable_bouncer: false,
        }
    }
}

impl Settings {
    pub(crate) fn from_pool_options(options: PoolOptions) -> Result<Self> {
        let chain = options.chain.unwrap_or(Chain::Mainnet);

        let settings = Self {
            address: options.address.unwrap_or_else(|| "0.0.0.0".into()),
            port: options.port.unwrap_or(42069),
            api_port: options.api_port,
            upstream_endpoint: None,
            upstream_username: None,
            upstream_password: None,
            timeout: Duration::from_secs(30),
            bitcoin_data_dir: options.bitcoin_data_dir,
            bitcoin_rpc_password: options.bitcoin_rpc_password,
            bitcoin_rpc_port: options
                .bitcoin_rpc_port
                .unwrap_or_else(|| chain.default_rpc_port()),
            bitcoin_rpc_username: options.bitcoin_rpc_username,
            bitcoin_rpc_cookie_file: options.bitcoin_rpc_cookie_file,
            chain,
            acme_domains: options.acme_domain,
            acme_contacts: options.acme_contact,
            acme_cache: options
                .acme_cache
                .unwrap_or_else(|| PathBuf::from("acme-cache")),
            data_dir: options.data_dir,
            update_interval: Duration::from_secs(options.update_interval.unwrap_or(10)),
            version_mask: options
                .version_mask
                .unwrap_or_else(|| "1fffe000".parse().unwrap()),
            start_diff: options.start_diff.unwrap_or_else(|| Difficulty::from(1.0)),
            min_diff: options.min_diff,
            max_diff: options.max_diff,
            vardiff_period: Duration::from_secs_f64(options.vardiff_period.unwrap_or(5.0)),
            vardiff_window: Duration::from_secs_f64(options.vardiff_window.unwrap_or(300.0)),
            zmq_block_notifications: options
                .zmq_block_notifications
                .unwrap_or_else(|| "tcp://127.0.0.1:28332".parse().unwrap()),
            extranonce2_size: options.extranonce2_size.unwrap_or(8),
            disable_bouncer: options.disable_bouncer,
        };

        settings.validate()?;
        Ok(settings)
    }

    pub(crate) fn from_proxy_options(options: ProxyOptions) -> Result<Self> {
        let settings = Self {
            address: options.address.unwrap_or_else(|| "0.0.0.0".into()),
            port: options.port.unwrap_or(42069),
            api_port: options.api_port,
            upstream_endpoint: Some(options.upstream),
            upstream_username: Some(options.username),
            upstream_password: options.password,
            timeout: Duration::from_secs(options.timeout.unwrap_or(30)),
            chain: options.chain.unwrap_or_default(),
            start_diff: options.start_diff.unwrap_or_else(|| Difficulty::from(1.0)),
            min_diff: options.min_diff,
            max_diff: options.max_diff,
            vardiff_period: Duration::from_secs_f64(options.vardiff_period.unwrap_or(5.0)),
            vardiff_window: Duration::from_secs_f64(options.vardiff_window.unwrap_or(300.0)),
            ..Default::default()
        };

        settings.validate()?;
        Ok(settings)
    }

    pub(crate) fn bitcoin_rpc_url(&self) -> String {
        format!("127.0.0.1:{}/", self.bitcoin_rpc_port)
    }

    pub(crate) fn bitcoin_credentials(&self) -> Result<Auth> {
        if let (Some(user), Some(pass)) = (&self.bitcoin_rpc_username, &self.bitcoin_rpc_password) {
            Ok(Auth::UserPass(user.clone(), pass.clone()))
        } else {
            Ok(Auth::CookieFile(self.cookie_file()?))
        }
    }

    pub(crate) fn cookie_file(&self) -> Result<PathBuf> {
        if let Some(cookie_file) = &self.bitcoin_rpc_cookie_file {
            return Ok(cookie_file.clone());
        }

        let path = if let Some(bitcoin_data_dir) = &self.bitcoin_data_dir {
            bitcoin_data_dir.clone()
        } else if cfg!(target_os = "linux") {
            dirs::home_dir()
                .ok_or_else(|| anyhow!("failed to get cookie file path: could not get home dir"))?
                .join(".bitcoin")
        } else {
            dirs::data_dir()
                .ok_or_else(|| anyhow!("failed to get cookie file path: could not get data dir"))?
                .join("Bitcoin")
        };

        let path = self.chain.join_with_data_dir(path);

        Ok(path.join(".cookie"))
    }

    pub(crate) fn bitcoin_rpc_client(&self) -> Result<bitcoincore_rpc::Client> {
        let rpc_url = self.bitcoin_rpc_url();

        let bitcoin_credentials = self.bitcoin_credentials()?;

        info!("Connecting to Bitcoin Core at {rpc_url}",);

        let client =
            bitcoincore_rpc::Client::new(&rpc_url, bitcoin_credentials.clone()).map_err(|_| {
                anyhow!(format!(
                    "failed to connect to Bitcoin Core RPC at `{rpc_url}` with {}",
                    match bitcoin_credentials {
                        Auth::None => "no credentials".into(),
                        Auth::UserPass(_, _) => "username and password".into(),
                        Auth::CookieFile(cookie_file) =>
                            format!("cookie file at {}", cookie_file.display()),
                    }
                ))
            })?;

        let mut checks = 0;
        let rpc_chain = loop {
            match client.get_blockchain_info() {
                Ok(blockchain_info) => {
                    break match blockchain_info.chain.to_string().as_str() {
                        "bitcoin" => Chain::Mainnet,
                        "regtest" => Chain::Regtest,
                        "signet" => Chain::Signet,
                        "testnet" => Chain::Testnet,
                        "testnet4" => Chain::Testnet4,
                        other => bail!("Bitcoin RPC server on unknown chain: {other}"),
                    };
                }
                Err(bitcoincore_rpc::Error::JsonRpc(bitcoincore_rpc::jsonrpc::Error::Rpc(err)))
                    if err.code == -28 => {}
                Err(err) => {
                    bail!("Failed to connect to Bitcoin Core RPC at `{rpc_url}`:  {err}")
                }
            }

            ensure! {
              checks < 100,
              "Failed to connect to Bitcoin Core RPC at `{rpc_url}`",
            }

            checks += 1;
            thread::sleep(Duration::from_millis(100));
        };

        let para_chain = self.chain;

        if rpc_chain != para_chain {
            bail!("Bitcoin RPC server is on {rpc_chain} but para is on {para_chain}");
        }

        Ok(client)
    }

    pub(crate) fn acme_cache_path(&self) -> PathBuf {
        if let Some(data_dir) = &self.data_dir {
            data_dir.join(&self.acme_cache)
        } else {
            self.acme_cache.clone()
        }
    }

    fn validate(&self) -> Result<()> {
        if let Some(min) = self.min_diff {
            ensure!(
                self.start_diff >= min,
                "start_diff ({}) must be >= min_diff ({})",
                self.start_diff,
                min
            );
        }

        if let Some(max) = self.max_diff {
            ensure!(
                self.start_diff <= max,
                "start_diff ({}) must be <= max_diff ({})",
                self.start_diff,
                max
            );
        }

        if let (Some(min), Some(max)) = (self.min_diff, self.max_diff) {
            ensure!(
                min <= max,
                "min_diff ({}) must be <= max_diff ({})",
                min,
                max
            );
        }

        Ok(())
    }

    pub(crate) fn address(&self) -> &str {
        &self.address
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn api_port(&self) -> Option<u16> {
        self.api_port
    }

    pub(crate) fn upstream(&self) -> Result<&str> {
        self.upstream_endpoint
            .as_deref()
            .context("upstream not configured (required for proxy mode)")
    }

    pub(crate) fn upstream_username(&self) -> Result<&Username> {
        self.upstream_username
            .as_ref()
            .context("upstream username not configured (required for proxy mode)")
    }

    pub(crate) fn upstream_password(&self) -> Option<String> {
        self.upstream_password.clone()
    }

    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }

    pub(crate) fn chain(&self) -> Chain {
        self.chain
    }

    pub(crate) fn acme_domains(&self) -> &[String] {
        &self.acme_domains
    }

    pub(crate) fn acme_contacts(&self) -> &[String] {
        &self.acme_contacts
    }

    pub(crate) fn update_interval(&self) -> Duration {
        self.update_interval
    }

    pub(crate) fn version_mask(&self) -> Version {
        self.version_mask
    }

    pub(crate) fn start_diff(&self) -> Difficulty {
        self.start_diff
    }

    pub(crate) fn min_diff(&self) -> Option<Difficulty> {
        self.min_diff
    }

    pub(crate) fn max_diff(&self) -> Option<Difficulty> {
        self.max_diff
    }

    pub(crate) fn vardiff_period(&self) -> Duration {
        self.vardiff_period
    }

    pub(crate) fn vardiff_window(&self) -> Duration {
        self.vardiff_window
    }

    pub(crate) fn zmq_block_notifications(&self) -> &Endpoint {
        &self.zmq_block_notifications
    }

    pub(crate) fn extranonce2_size(&self) -> usize {
        self.extranonce2_size as usize
    }

    pub(crate) fn disable_bouncer(&self) -> bool {
        self.disable_bouncer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arguments::Arguments;

    fn parse_pool_options(args: &str) -> PoolOptions {
        match Arguments::try_parse_from(args.split_whitespace()) {
            Ok(arguments) => match arguments.subcommand {
                crate::subcommand::Subcommand::Pool(pool) => pool.options,
                subcommand => panic!("unexpected subcommand: {subcommand:?}"),
            },
            Err(err) => panic!("error parsing arguments: {err}"),
        }
    }

    fn parse_proxy_options(args: &str) -> ProxyOptions {
        match Arguments::try_parse_from(args.split_whitespace()) {
            Ok(arguments) => match arguments.subcommand {
                crate::subcommand::Subcommand::Proxy(proxy) => proxy.options,
                subcommand => panic!("unexpected subcommand: {subcommand:?}"),
            },
            Err(err) => panic!("error parsing arguments: {err}"),
        }
    }

    #[test]
    fn pool_defaults_are_sane() {
        let options = parse_pool_options("para pool");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(settings.address, "0.0.0.0");
        assert_eq!(settings.port, 42069);
        assert_eq!(settings.chain, Chain::Mainnet);
        assert_eq!(settings.bitcoin_rpc_port, settings.chain.default_rpc_port());
        assert_eq!(
            settings.bitcoin_rpc_url(),
            format!("127.0.0.1:{}/", settings.bitcoin_rpc_port)
        );
        assert_eq!(
            settings.version_mask,
            Version::from_str("1fffe000").unwrap()
        );
        assert_eq!(settings.update_interval, Duration::from_secs(10));
        assert_eq!(
            settings.zmq_block_notifications.to_string(),
            "tcp://127.0.0.1:28332".to_string()
        );
        assert_eq!(settings.extranonce2_size, 8);
    }

    #[test]
    fn pool_override_address_and_port() {
        let options = parse_pool_options("para pool --address 127.0.0.1 --port 9999");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(settings.address, "127.0.0.1");
        assert_eq!(settings.port, 9999);
    }

    #[test]
    fn pool_override_chain_changes_default_rpc_port() {
        let options = parse_pool_options("para pool --chain signet");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(settings.chain, Chain::Signet);
        assert_eq!(settings.bitcoin_rpc_port, settings.chain.default_rpc_port());
    }

    #[test]
    fn pool_explicit_bitcoin_rpc_port_wins() {
        let options = parse_pool_options("para pool --chain regtest --bitcoin-rpc-port 4242");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(settings.chain, Chain::Regtest);
        assert_eq!(settings.bitcoin_rpc_port, 4242);
        assert_eq!(settings.bitcoin_rpc_url(), "127.0.0.1:4242/");
    }

    #[test]
    fn pool_override_version_mask() {
        let options = parse_pool_options("para pool --version-mask 00fff000");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(
            settings.version_mask,
            Version::from_str("00fff000").unwrap()
        );
    }

    #[test]
    fn pool_credentials_userpass_when_both_provided() {
        let options = parse_pool_options(
            "para pool \
                --bitcoin-rpc-username alice --bitcoin-rpc-password secret \
                --bitcoin-rpc-cookie-file /dev/null/.cookie",
        );
        let settings = Settings::from_pool_options(options).unwrap();

        match settings.bitcoin_credentials().unwrap() {
            Auth::UserPass(username, password) => {
                assert_eq!(username, "alice");
                assert_eq!(password, "secret");
            }
            other => panic!("expected UserPass, got {other:?}"),
        }
    }

    #[test]
    fn pool_credentials_fallback_to_cookie_when_partial_creds() {
        let options = parse_pool_options(
            "para pool \
                --bitcoin-rpc-username onlyuser \
                --bitcoin-rpc-cookie-file /tmp/test.cookie",
        );
        let settings = Settings::from_pool_options(options).unwrap();

        match settings.bitcoin_credentials().unwrap() {
            Auth::CookieFile(path) => assert_eq!(path, PathBuf::from("/tmp/test.cookie")),
            other => panic!("expected CookieFile, got {other:?}"),
        }
    }

    #[test]
    fn pool_credentials_cookiefile_when_no_creds() {
        let options =
            parse_pool_options("para pool --bitcoin-rpc-cookie-file /var/lib/bitcoind/.cookie");
        let settings = Settings::from_pool_options(options).unwrap();

        match settings.bitcoin_credentials().unwrap() {
            Auth::CookieFile(path) => assert_eq!(path, PathBuf::from("/var/lib/bitcoind/.cookie")),
            other => panic!("expected CookieFile, got {other:?}"),
        }
    }

    #[test]
    fn pool_cookie_file_from_explicit_cookie_path() {
        let options = parse_pool_options("para pool --bitcoin-rpc-cookie-file /x/y/.cookie");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(
            settings.cookie_file().unwrap(),
            PathBuf::from("/x/y/.cookie")
        );
    }

    #[test]
    fn pool_cookie_file_from_bitcoin_data_dir_and_chain() {
        let options =
            parse_pool_options("para pool --bitcoin-data-dir /data/bitcoin --chain regtest");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(
            settings.cookie_file().unwrap(),
            PathBuf::from("/data/bitcoin/regtest/.cookie")
        );

        let options =
            parse_pool_options("para pool --bitcoin-data-dir /data/bitcoin --chain signet");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(
            settings.cookie_file().unwrap(),
            PathBuf::from("/data/bitcoin/signet/.cookie")
        );

        let options =
            parse_pool_options("para pool --bitcoin-data-dir /data/bitcoin --chain mainnet");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(
            settings.cookie_file().unwrap(),
            PathBuf::from("/data/bitcoin/.cookie")
        );
    }

    #[test]
    fn pool_rpc_url_reflects_port_choice() {
        let options = parse_pool_options("para pool --bitcoin-rpc-port 12345");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(settings.bitcoin_rpc_url(), "127.0.0.1:12345/");
    }

    #[test]
    fn pool_zmq_block_notifications() {
        let options = parse_pool_options("para pool --zmq-block-notifications tcp://127.0.0.1:69");
        let settings = Settings::from_pool_options(options).unwrap();

        assert_eq!(
            settings.zmq_block_notifications,
            "tcp://127.0.0.1:69".parse().unwrap()
        );
    }

    #[test]
    fn pool_start_diff() {
        let options = parse_pool_options("para pool --start-diff 0.00001");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.start_diff, Difficulty::from(0.00001));

        let options = parse_pool_options("para pool --start-diff 111");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.start_diff, Difficulty::from(111));

        let options = parse_pool_options("para pool");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.start_diff, Difficulty::from(1));
    }

    #[test]
    fn pool_vardiff_period() {
        let options = parse_pool_options("para pool --vardiff-period 10.0");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.vardiff_period, Duration::from_secs(10));

        let options = parse_pool_options("para pool --vardiff-period 0.5");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.vardiff_period, Duration::from_millis(500));

        let options = parse_pool_options("para pool");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.vardiff_period, Duration::from_secs(5));
    }

    #[test]
    fn pool_vardiff_window() {
        let options = parse_pool_options("para pool --vardiff-window 60");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.vardiff_window, Duration::from_secs(60));

        let options = parse_pool_options("para pool --vardiff-window 600.5");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.vardiff_window, Duration::from_secs_f64(600.5));

        let options = parse_pool_options("para pool");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.vardiff_window, Duration::from_secs(300));
    }

    #[test]
    fn pool_extranonce2_size_default() {
        let options = parse_pool_options("para pool");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.extranonce2_size, 8);
    }

    #[test]
    fn pool_extranonce2_size_override() {
        let options = parse_pool_options("para pool --extranonce2-size 4");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.extranonce2_size, 4);
    }

    #[test]
    fn pool_extranonce2_size_boundaries() {
        let options = parse_pool_options("para pool --extranonce2-size 2");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.extranonce2_size, 2);

        let options = parse_pool_options("para pool --extranonce2-size 8");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.extranonce2_size, 8);
    }

    #[test]
    #[should_panic(expected = "error parsing arguments")]
    fn pool_extranonce2_size_too_small() {
        parse_pool_options("para pool --extranonce2-size 1");
    }

    #[test]
    #[should_panic(expected = "error parsing arguments")]
    fn pool_extranonce2_size_too_large() {
        parse_pool_options("para pool --extranonce2-size 9");
    }

    #[test]
    fn pool_min_diff_parsing() {
        let options = parse_pool_options("para pool --min-diff 0.001");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.min_diff, Some(Difficulty::from(0.001)));
    }

    #[test]
    fn pool_max_diff_parsing() {
        let options = parse_pool_options("para pool --max-diff 1000");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.max_diff, Some(Difficulty::from(1000)));
    }

    #[test]
    fn pool_min_max_diff_not_set_by_default() {
        let options = parse_pool_options("para pool");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.min_diff, None);
        assert_eq!(settings.max_diff, None);
    }

    #[test]
    fn pool_valid_min_max_diff_config() {
        let options = parse_pool_options("para pool --start-diff 10 --min-diff 1 --max-diff 100");
        assert!(Settings::from_pool_options(options).is_ok());
    }

    #[test]
    fn pool_start_diff_below_min_diff_fails() {
        let options = parse_pool_options("para pool --start-diff 1 --min-diff 10");
        assert!(Settings::from_pool_options(options).is_err());
    }

    #[test]
    fn pool_start_diff_above_max_diff_fails() {
        let options = parse_pool_options("para pool --start-diff 100 --max-diff 10");
        assert!(Settings::from_pool_options(options).is_err());
    }

    #[test]
    fn pool_min_diff_above_max_diff_fails() {
        let options = parse_pool_options("para pool --start-diff 50 --min-diff 100 --max-diff 10");
        assert!(Settings::from_pool_options(options).is_err());
    }

    #[test]
    fn proxy_defaults_are_sane() {
        let options =
            parse_proxy_options("para proxy --upstream pool.example.com:3333 --username bc1qtest");
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(
            settings.upstream_endpoint,
            Some("pool.example.com:3333".into())
        );
        assert_eq!(
            settings.upstream_username.as_ref().map(|u| u.to_string()),
            Some("bc1qtest".into())
        );
        assert_eq!(settings.upstream_password, None);
        assert_eq!(settings.address, "0.0.0.0");
        assert_eq!(settings.port, 42069);
        assert_eq!(settings.api_port, None);
        assert_eq!(settings.timeout, Duration::from_secs(30));
    }

    #[test]
    fn proxy_override_address_and_port() {
        let options = parse_proxy_options(
            "para proxy --upstream pool.example.com:3333 --username bc1qtest --address 127.0.0.1 --port 9999",
        );
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(settings.address, "127.0.0.1");
        assert_eq!(settings.port, 9999);
    }

    #[test]
    fn proxy_override_api_port() {
        let options = parse_proxy_options(
            "para proxy --upstream pool.example.com:3333 --username bc1qtest --api-port 8080",
        );
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(settings.api_port, Some(8080));
    }

    #[test]
    fn proxy_override_timeout() {
        let options = parse_proxy_options(
            "para proxy --upstream pool.example.com:3333 --username bc1qtest --timeout 60",
        );
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(settings.timeout, Duration::from_secs(60));
    }

    #[test]
    fn proxy_password_override() {
        let options = parse_proxy_options(
            "para proxy --upstream pool.example.com:3333 --username bc1qtest --password secret",
        );
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(settings.upstream_password, Some("secret".to_string()));
    }

    #[test]
    fn proxy_username_with_worker() {
        let options = parse_proxy_options(
            "para proxy --upstream pool.example.com:3333 --username bc1qtest.worker1",
        );
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(
            settings.upstream_username.as_ref().map(|u| u.to_string()),
            Some("bc1qtest.worker1".into())
        );
    }
}
