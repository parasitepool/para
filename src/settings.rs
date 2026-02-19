use super::*;

mod common_options;
mod pool_options;
mod proxy_options;

pub(crate) use {
    common_options::CommonOptions, pool_options::PoolOptions, proxy_options::ProxyOptions,
};

#[derive(Clone, Debug)]
pub(crate) struct Settings {
    address: String,
    port: u16,
    http_port: Option<u16>,
    upstream_endpoint: Option<String>,
    upstream_username: Option<Username>,
    upstream_password: Option<String>,
    timeout: Duration,
    bitcoin_data_dir: Option<PathBuf>,
    bitcoin_rpc_port: u16,
    bitcoin_rpc_cookie_file: Option<PathBuf>,
    bitcoin_rpc_username: Option<String>,
    bitcoin_rpc_password: Option<String>,
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
    enonce1_size: usize,
    enonce2_size: usize,
    enonce1_extension_size: usize,
    disable_bouncer: bool,
    database_url: Option<String>,
    events_file: Option<PathBuf>,
    high_diff_port: Option<u16>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            address: "0.0.0.0".into(),
            port: 42069,
            http_port: None,
            upstream_endpoint: None,
            upstream_username: None,
            upstream_password: None,
            timeout: Duration::from_secs(30),
            bitcoin_data_dir: None,
            bitcoin_rpc_port: Chain::default().default_rpc_port(),
            bitcoin_rpc_cookie_file: None,
            bitcoin_rpc_username: None,
            bitcoin_rpc_password: None,
            chain: Chain::default(),
            acme_domains: vec![],
            acme_contacts: vec![],
            acme_cache: PathBuf::from("acme-cache"),
            data_dir: None,
            update_interval: Duration::from_secs(10),
            version_mask: Version::default(),
            start_diff: Difficulty::default(),
            min_diff: None,
            max_diff: None,
            vardiff_period: Duration::from_secs_f64(3.33),
            vardiff_window: Duration::from_secs(300),
            zmq_block_notifications: "tcp://127.0.0.1:28332".parse().unwrap(),
            enonce1_size: ENONCE1_SIZE,
            enonce2_size: MAX_ENONCE_SIZE,
            enonce1_extension_size: ENONCE1_EXTENSION_SIZE,
            disable_bouncer: false,
            database_url: None,
            events_file: None,
            high_diff_port: None,
        }
    }
}

impl Settings {
    pub(crate) fn from_pool_options(options: PoolOptions) -> Result<Self> {
        let chain = options.common.chain.unwrap_or_default();

        let bitcoin_rpc_port = options
            .common
            .bitcoin_rpc_port
            .unwrap_or_else(|| chain.default_rpc_port());

        let settings = Self {
            address: options.common.address,
            port: options.common.port,
            http_port: options.common.http_port,
            upstream_endpoint: None,
            upstream_username: None,
            upstream_password: None,
            timeout: Duration::from_secs(30),
            bitcoin_data_dir: options.common.bitcoin_data_dir,
            bitcoin_rpc_port,
            bitcoin_rpc_cookie_file: options.common.bitcoin_rpc_cookie_file,
            bitcoin_rpc_username: options.common.bitcoin_rpc_username,
            bitcoin_rpc_password: options.common.bitcoin_rpc_password,
            chain,
            acme_domains: options.common.acme_domain,
            acme_contacts: options.common.acme_contact,
            acme_cache: options.common.acme_cache,
            data_dir: options.common.data_dir,
            update_interval: Duration::from_secs(options.update_interval),
            version_mask: options.version_mask,
            start_diff: options.common.start_diff,
            min_diff: options.common.min_diff,
            max_diff: options.common.max_diff,
            vardiff_period: Duration::from_secs_f64(options.common.vardiff_period),
            vardiff_window: Duration::from_secs_f64(options.common.vardiff_window),
            zmq_block_notifications: options.zmq_block_notifications,
            enonce1_size: options.enonce1_size,
            enonce2_size: options.enonce2_size,
            enonce1_extension_size: ENONCE1_EXTENSION_SIZE,
            disable_bouncer: options.disable_bouncer,
            database_url: options.database_url,
            events_file: options.events_file,
            high_diff_port: options.common.high_diff_port,
        };

        settings.validate()?;
        Ok(settings)
    }

    pub(crate) fn from_proxy_options(options: ProxyOptions) -> Result<Self> {
        let chain = options.common.chain.unwrap_or_default();

        let bitcoin_rpc_port = options
            .common
            .bitcoin_rpc_port
            .unwrap_or_else(|| chain.default_rpc_port());

        let settings = Self {
            address: options.common.address,
            port: options.common.port,
            http_port: options.common.http_port,
            upstream_endpoint: Some(options.upstream),
            upstream_username: Some(options.username),
            upstream_password: options.password,
            timeout: Duration::from_secs(options.timeout),
            bitcoin_data_dir: options.common.bitcoin_data_dir,
            bitcoin_rpc_port,
            bitcoin_rpc_cookie_file: options.common.bitcoin_rpc_cookie_file,
            bitcoin_rpc_username: options.common.bitcoin_rpc_username,
            bitcoin_rpc_password: options.common.bitcoin_rpc_password,
            chain,
            acme_domains: options.common.acme_domain,
            acme_contacts: options.common.acme_contact,
            acme_cache: options.common.acme_cache,
            data_dir: options.common.data_dir,
            update_interval: Duration::from_secs(10),
            version_mask: Version::default(),
            start_diff: options.common.start_diff,
            min_diff: options.common.min_diff,
            max_diff: options.common.max_diff,
            vardiff_period: Duration::from_secs_f64(options.common.vardiff_period),
            vardiff_window: Duration::from_secs_f64(options.common.vardiff_window),
            zmq_block_notifications: "tcp://127.0.0.1:28332".parse().unwrap(),
            enonce1_size: ENONCE1_SIZE,
            enonce2_size: MAX_ENONCE_SIZE,
            enonce1_extension_size: options.enonce1_extension_size,
            disable_bouncer: false,
            database_url: None,
            events_file: None,
            high_diff_port: options.common.high_diff_port,
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

    pub(crate) async fn bitcoin_rpc_client(&self) -> Result<Client> {
        let rpc_url = format!("http://{}", self.bitcoin_rpc_url());

        let bitcoin_credentials = self.bitcoin_credentials()?;

        info!("Connecting to Bitcoin Core at {rpc_url}");

        let client = Client::new(
            rpc_url.clone(),
            bitcoin_credentials.clone(),
            None,
            None,
            None,
        )
        .map_err(|err| {
            anyhow!(format!(
                "failed to connect to Bitcoin Core RPC at `{rpc_url}` with {} and error: {err}",
                match bitcoin_credentials {
                    Auth::UserPass(_, _) => "username and password".into(),
                    Auth::CookieFile(cookie_file) =>
                        format!("cookie file at {}", cookie_file.display()),
                }
            ))
        })?;

        let mut checks = 0;
        let rpc_chain = loop {
            match client.get_blockchain_info().await {
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
                Err(bitcoind_async_client::error::ClientError::Server(-28, _)) => {}
                Err(err) => {
                    bail!("Failed to connect to Bitcoin Core RPC at `{rpc_url}`: {err}")
                }
            }

            ensure! {
                checks < 100,
                "Failed to connect to Bitcoin Core RPC at `{rpc_url}`",
            }

            checks += 1;
            sleep(Duration::from_millis(100)).await;
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

        ensure!(
            self.enonce1_size >= MIN_ENONCE_SIZE,
            "enonce1_size ({}) must be >= {}",
            self.enonce1_size,
            MIN_ENONCE_SIZE
        );

        ensure!(
            self.enonce1_size <= MAX_ENONCE_SIZE,
            "enonce1_size ({}) must be <= {}",
            self.enonce1_size,
            MAX_ENONCE_SIZE
        );

        ensure!(
            self.enonce2_size >= MIN_ENONCE_SIZE,
            "enonce2_size ({}) must be >= {}",
            self.enonce2_size,
            MIN_ENONCE_SIZE
        );

        ensure!(
            self.enonce2_size <= MAX_ENONCE_SIZE,
            "enonce2_size ({}) must be <= {}",
            self.enonce2_size,
            MAX_ENONCE_SIZE
        );

        ensure!(
            self.enonce1_extension_size >= 1,
            "enonce1_extension_size ({}) must be >= 1",
            self.enonce1_extension_size
        );

        ensure!(
            self.enonce1_extension_size <= MAX_ENONCE_SIZE - MIN_ENONCE_SIZE,
            "enonce1_extension_size ({}) must be <= {}",
            self.enonce1_extension_size,
            MAX_ENONCE_SIZE - MIN_ENONCE_SIZE
        );

        if let Some(high_diff_port) = self.high_diff_port {
            ensure!(
                high_diff_port != self.port,
                "high_diff_port ({}) must not equal port ({})",
                high_diff_port,
                self.port
            );

            if let Some(http_port) = self.http_port {
                ensure!(
                    high_diff_port != http_port,
                    "high_diff_port ({}) must not equal http_port ({})",
                    high_diff_port,
                    http_port
                );
            }

            if let Some(max) = self.max_diff {
                ensure!(
                    self.high_diff_start() <= max,
                    "high_diff_port start difficulty ({}) must be <= max_diff ({})",
                    self.high_diff_start(),
                    max
                );
            }

            if let Some(min) = self.min_diff {
                ensure!(
                    self.high_diff_start() >= min,
                    "high_diff_port start difficulty ({}) must be >= min_diff ({})",
                    self.high_diff_start(),
                    min
                );
            }
        }

        Ok(())
    }

    pub(crate) fn address(&self) -> &str {
        &self.address
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn http_port(&self) -> Option<u16> {
        self.http_port
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

    pub(crate) fn enonce1_size(&self) -> usize {
        self.enonce1_size
    }

    pub(crate) fn enonce2_size(&self) -> usize {
        self.enonce2_size
    }

    pub(crate) fn enonce1_extension_size(&self) -> usize {
        self.enonce1_extension_size
    }

    pub(crate) fn disable_bouncer(&self) -> bool {
        self.disable_bouncer
    }

    pub(crate) fn database_url(&self) -> Option<String> {
        self.database_url.clone()
    }

    pub(crate) fn events_file(&self) -> Option<PathBuf> {
        self.events_file.clone()
    }

    pub(crate) fn high_diff_port(&self) -> Option<u16> {
        self.high_diff_port
    }

    pub(crate) fn high_diff_start(&self) -> Difficulty {
        Difficulty::from(1_000_000)
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::arguments::Arguments};

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
        assert_eq!(settings.enonce1_size, ENONCE1_SIZE);
        assert_eq!(settings.enonce2_size, MAX_ENONCE_SIZE);
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
        assert_eq!(settings.vardiff_period, Duration::from_secs_f64(3.33));
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
    fn pool_enonce1_size_default() {
        let options = parse_pool_options("para pool");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.enonce1_size, ENONCE1_SIZE);
    }

    #[test]
    fn pool_enonce1_size_override() {
        let options = parse_pool_options("para pool --enonce1-size 6");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.enonce1_size, 6);
    }

    #[test]
    fn pool_enonce1_size_boundaries() {
        let options = parse_pool_options("para pool --enonce1-size 2");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.enonce1_size, 2);

        let options = parse_pool_options("para pool --enonce1-size 8");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.enonce1_size, 8);
    }

    #[test]
    fn pool_enonce1_size_too_small() {
        let options = parse_pool_options("para pool --enonce1-size 1");
        let err = Settings::from_pool_options(options).unwrap_err();
        assert!(err.to_string().contains("enonce1_size (1) must be >="));
    }

    #[test]
    fn pool_enonce1_size_too_large() {
        let options = parse_pool_options("para pool --enonce1-size 9");
        let err = Settings::from_pool_options(options).unwrap_err();
        assert!(err.to_string().contains("enonce1_size (9) must be <="));
    }

    #[test]
    fn pool_enonce2_size_default() {
        let options = parse_pool_options("para pool");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.enonce2_size, MAX_ENONCE_SIZE);
    }

    #[test]
    fn pool_enonce2_size_override() {
        let options = parse_pool_options("para pool --enonce2-size 4");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.enonce2_size, 4);
    }

    #[test]
    fn pool_enonce2_size_boundaries() {
        let options = parse_pool_options("para pool --enonce2-size 2");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.enonce2_size, 2);

        let options = parse_pool_options("para pool --enonce2-size 8");
        let settings = Settings::from_pool_options(options).unwrap();
        assert_eq!(settings.enonce2_size, 8);
    }

    #[test]
    fn pool_enonce2_size_too_small() {
        let options = parse_pool_options("para pool --enonce2-size 1");
        let err = Settings::from_pool_options(options).unwrap_err();
        assert!(err.to_string().contains("enonce2_size (1) must be >="));
    }

    #[test]
    fn pool_enonce2_size_too_large() {
        let options = parse_pool_options("para pool --enonce2-size 9");
        let err = Settings::from_pool_options(options).unwrap_err();
        assert!(err.to_string().contains("enonce2_size (9) must be <="));
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
    fn acme_options() {
        #[track_caller]
        fn case(pool_args: &str, proxy_args: &str, check: impl Fn(&Settings)) {
            let pool = Settings::from_pool_options(parse_pool_options(pool_args)).unwrap();
            let proxy = Settings::from_proxy_options(parse_proxy_options(proxy_args)).unwrap();
            check(&pool);
            check(&proxy);
        }

        case(
            "para pool",
            "para proxy --upstream foo:1234 --username bar",
            |s| {
                assert!(s.acme_domains.is_empty());
                assert!(s.acme_contacts.is_empty());
                assert_eq!(s.acme_cache, PathBuf::from("acme-cache"));
            },
        );

        case(
            "para pool --acme-domain foo.bar --acme-domain baz.qux",
            "para proxy --upstream foo:1234 --username bar --acme-domain foo.bar --acme-domain baz.qux",
            |s| {
                assert_eq!(s.acme_domains, vec!["foo.bar", "baz.qux"]);
            },
        );

        case(
            "para pool --acme-contact foo@bar --acme-contact baz@qux",
            "para proxy --upstream foo:1234 --username bar --acme-contact foo@bar --acme-contact baz@qux",
            |s| {
                assert_eq!(s.acme_contacts, vec!["foo@bar", "baz@qux"]);
            },
        );

        case(
            "para pool --acme-cache /foo/bar",
            "para proxy --upstream foo:1234 --username bar --acme-cache /foo/bar",
            |s| {
                assert_eq!(s.acme_cache, PathBuf::from("/foo/bar"));
            },
        );

        case(
            "para pool --data-dir /foo",
            "para proxy --upstream foo:1234 --username bar --data-dir /foo",
            |s| {
                assert_eq!(s.acme_cache_path(), PathBuf::from("/foo/acme-cache"));
            },
        );
    }

    #[test]
    fn proxy_defaults_are_sane() {
        let options = parse_proxy_options("para proxy --upstream foo:1234 --username bar");
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(settings.upstream_endpoint, Some("foo:1234".into()));
        assert_eq!(
            settings.upstream_username.as_ref().map(|u| u.to_string()),
            Some("bar".into())
        );
        assert_eq!(settings.upstream_password, None);
        assert_eq!(settings.address, "0.0.0.0");
        assert_eq!(settings.port, 42069);
        assert_eq!(settings.http_port, None);
        assert_eq!(settings.timeout, Duration::from_secs(30));
    }

    #[test]
    fn proxy_override_address_and_port() {
        let options = parse_proxy_options(
            "para proxy --upstream foo:1234 --username bar --address 127.0.0.1 --port 9999",
        );
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(settings.address, "127.0.0.1");
        assert_eq!(settings.port, 9999);
    }

    #[test]
    fn proxy_override_http_port() {
        let options =
            parse_proxy_options("para proxy --upstream foo:1234 --username bar --http-port 8080");
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(settings.http_port, Some(8080));
    }

    #[test]
    fn proxy_override_timeout() {
        let options =
            parse_proxy_options("para proxy --upstream foo:1234 --username bar --timeout 60");
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(settings.timeout, Duration::from_secs(60));
    }

    #[test]
    fn proxy_password_override() {
        let options =
            parse_proxy_options("para proxy --upstream foo:1234 --username bar --password baz");
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(settings.upstream_password, Some("baz".into()));
    }

    #[test]
    fn proxy_username_with_worker() {
        let options = parse_proxy_options("para proxy --upstream foo:1234 --username bar.baz");
        let settings = Settings::from_proxy_options(options).unwrap();

        assert_eq!(
            settings.upstream_username.as_ref().map(|u| u.to_string()),
            Some("bar.baz".into())
        );
    }

    #[test]
    fn proxy_enonce1_extension_size_default() {
        let options = parse_proxy_options("para proxy --upstream foo:1234 --username bar");
        let settings = Settings::from_proxy_options(options).unwrap();
        assert_eq!(settings.enonce1_extension_size, ENONCE1_EXTENSION_SIZE);
    }

    #[test]
    fn proxy_enonce1_extension_size_override() {
        let options = parse_proxy_options(
            "para proxy --upstream foo:1234 --username bar --enonce1-extension-size 3",
        );
        let settings = Settings::from_proxy_options(options).unwrap();
        assert_eq!(settings.enonce1_extension_size, 3);
    }

    #[test]
    fn proxy_enonce1_extension_size_boundaries() {
        let options = parse_proxy_options(
            "para proxy --upstream foo:1234 --username bar --enonce1-extension-size 1",
        );
        let settings = Settings::from_proxy_options(options).unwrap();
        assert_eq!(settings.enonce1_extension_size, 1);

        let options = parse_proxy_options(
            "para proxy --upstream foo:1234 --username bar --enonce1-extension-size 6",
        );
        let settings = Settings::from_proxy_options(options).unwrap();
        assert_eq!(settings.enonce1_extension_size, 6);
    }

    #[test]
    fn proxy_enonce1_extension_size_too_small() {
        let options = parse_proxy_options(
            "para proxy --upstream foo:1234 --username bar --enonce1-extension-size 0",
        );
        let err = Settings::from_proxy_options(options).unwrap_err();
        assert!(
            err.to_string()
                .contains("enonce1_extension_size (0) must be >= 1")
        );
    }

    #[test]
    fn proxy_enonce1_extension_size_too_large() {
        let options = parse_proxy_options(
            "para proxy --upstream foo:1234 --username bar --enonce1-extension-size 7",
        );
        let err = Settings::from_proxy_options(options).unwrap_err();
        assert!(
            err.to_string()
                .contains("enonce1_extension_size (7) must be <=")
        );
    }

    #[test]
    fn high_diff_port() {
        #[track_caller]
        fn case(pool_args: &str, proxy_args: &str, expected: Option<u16>) {
            let pool = Settings::from_pool_options(parse_pool_options(pool_args)).unwrap();
            let proxy = Settings::from_proxy_options(parse_proxy_options(proxy_args)).unwrap();
            assert_eq!(pool.high_diff_port, expected);
            assert_eq!(proxy.high_diff_port, expected);
        }

        case(
            "para pool",
            "para proxy --upstream foo:1234 --username bar",
            None,
        );

        case(
            "para pool --high-diff-port 3333",
            "para proxy --upstream foo:1234 --username bar --high-diff-port 3333",
            Some(3333),
        );
    }

    #[test]
    fn high_diff_port_equals_port_fails() {
        let pool = parse_pool_options("para pool --port 3333 --high-diff-port 3333");
        let proxy = parse_proxy_options(
            "para proxy --upstream foo:1234 --username bar --port 3333 --high-diff-port 3333",
        );
        assert!(
            Settings::from_pool_options(pool)
                .unwrap_err()
                .to_string()
                .contains("must not equal port")
        );
        assert!(
            Settings::from_proxy_options(proxy)
                .unwrap_err()
                .to_string()
                .contains("must not equal port")
        );
    }

    #[test]
    fn high_diff_port_equals_http_port_fails() {
        let pool = parse_pool_options("para pool --http-port 8080 --high-diff-port 8080");
        let proxy = parse_proxy_options(
            "para proxy --upstream foo:1234 --username bar --http-port 8080 --high-diff-port 8080",
        );
        assert!(
            Settings::from_pool_options(pool)
                .unwrap_err()
                .to_string()
                .contains("must not equal http_port")
        );
        assert!(
            Settings::from_proxy_options(proxy)
                .unwrap_err()
                .to_string()
                .contains("must not equal http_port")
        );
    }

    #[test]
    fn high_diff_port_above_max_diff_fails() {
        let pool = parse_pool_options("para pool --high-diff-port 3333 --max-diff 100");
        let proxy = parse_proxy_options(
            "para proxy --upstream foo:1234 --username bar --high-diff-port 3333 --max-diff 100",
        );
        assert!(
            Settings::from_pool_options(pool)
                .unwrap_err()
                .to_string()
                .contains("high_diff_port start difficulty")
        );
        assert!(
            Settings::from_proxy_options(proxy)
                .unwrap_err()
                .to_string()
                .contains("high_diff_port start difficulty")
        );
    }

    #[test]
    fn high_diff_port_below_min_diff_fails() {
        let pool = parse_pool_options(
            "para pool --high-diff-port 3333 --min-diff 2000000 --start-diff 2000000",
        );
        let proxy = parse_proxy_options(
            "para proxy --upstream foo:1234 --username bar --high-diff-port 3333 --min-diff 2000000 --start-diff 2000000",
        );
        assert!(
            Settings::from_pool_options(pool)
                .unwrap_err()
                .to_string()
                .contains("high_diff_port start difficulty")
        );
        assert!(
            Settings::from_proxy_options(proxy)
                .unwrap_err()
                .to_string()
                .contains("high_diff_port start difficulty")
        );
    }
}
