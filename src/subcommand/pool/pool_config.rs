use {super::*, settings::Settings};

/// CLI arguments for pool subcommand
#[derive(Clone, Debug, Parser)]
pub(crate) struct PoolConfig {
    #[arg(long, help = "Listen at <ADDRESS>.")]
    pub address: Option<String>,
    #[arg(long, help = "Listen on <PORT>.")]
    pub port: Option<u16>,
    #[arg(long, help = "Block template <UPDATE_INTERVAL> in seconds.")]
    pub update_interval: Option<u64>,
    #[arg(long, help = "Use version rolling with <VERSION_MASK>.")]
    pub version_mask: Option<String>,
    #[arg(long, help = "Give <STARTING_DIFFICULTY> to new clients.")]
    pub start_diff: Option<String>,
    #[arg(
        long,
        help = "Target <VARDIFF_PERIOD> seconds between share submissions."
    )]
    pub vardiff_period: Option<f64>,
    #[arg(
        long,
        help = "Average the share submission rate over <VARDIFF_WINDOW> seconds."
    )]
    pub vardiff_window: Option<f64>,
    #[arg(long, help = "Subscribe to <ZMQ_BLOCK_NOTIFICATION>.")]
    pub zmq_block_notifications: Option<String>,
}

impl PoolConfig {
    pub fn address(&self, settings: &Settings) -> String {
        self.address
            .clone()
            .or(settings.pool_address.clone())
            .unwrap_or_else(|| "0.0.0.0".into())
    }

    pub fn port(&self, settings: &Settings) -> u16 {
        self.port.or(settings.pool_port).unwrap_or(42069)
    }

    pub fn update_interval(&self, settings: &Settings) -> Duration {
        Duration::from_secs(
            self.update_interval
                .or(settings.pool_update_interval)
                .unwrap_or(10),
        )
    }

    pub fn version_mask(&self, settings: &Settings) -> Version {
        let mask_str = self
            .version_mask
            .clone()
            .or(settings.pool_version_mask.clone())
            .unwrap_or_else(|| "1fffe000".into());
        Version::from_str(&mask_str).unwrap_or_else(|_| Version::from_str("1fffe000").unwrap())
    }

    pub fn start_diff(&self, settings: &Settings) -> Difficulty {
        let diff_str = self
            .start_diff
            .clone()
            .or(settings.pool_start_diff.clone())
            .unwrap_or_else(|| "1".into());
        Difficulty::from_str(&diff_str).unwrap_or(Difficulty::from(1.0))
    }

    pub fn vardiff_period(&self, settings: &Settings) -> Duration {
        Duration::from_secs_f64(
            self.vardiff_period
                .or(settings.pool_vardiff_period)
                .unwrap_or(5.0),
        )
    }

    pub fn vardiff_window(&self, settings: &Settings) -> Duration {
        Duration::from_secs_f64(
            self.vardiff_window
                .or(settings.pool_vardiff_window)
                .unwrap_or(300.0),
        )
    }

    pub fn zmq_block_notifications(&self, settings: &Settings) -> Endpoint {
        let endpoint_str = self
            .zmq_block_notifications
            .clone()
            .or(settings.pool_zmq_block_notifications.clone())
            .unwrap_or_else(|| "tcp://127.0.0.1:28332".into());
        endpoint_str
            .parse()
            .unwrap_or_else(|_| "tcp://127.0.0.1:28332".parse().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_pool_config(args: &str) -> PoolConfig {
        match crate::arguments::Arguments::try_parse_from(args.split_whitespace()) {
            Ok(arguments) => match arguments.subcommand {
                crate::subcommand::Subcommand::Pool(pool) => pool.config,
                subcommand => panic!("unexpected subcommand: {subcommand:?}"),
            },
            Err(err) => panic!("error parsing arguments: {err}"),
        }
    }

    fn default_settings() -> Settings {
        Settings::merge(crate::options::Options::default(), Default::default()).unwrap()
    }

    #[test]
    fn defaults_are_sane() {
        let config = parse_pool_config("para pool");
        let settings = default_settings();

        assert_eq!(config.address(&settings), "0.0.0.0");
        assert_eq!(config.port(&settings), 42069);
        assert_eq!(settings.chain(), Chain::Mainnet);
        assert_eq!(
            config.version_mask(&settings),
            Version::from_str("1fffe000").unwrap()
        );
        assert_eq!(config.update_interval(&settings), Duration::from_secs(10));
        assert_eq!(
            config.zmq_block_notifications(&settings).to_string(),
            "tcp://127.0.0.1:28332".to_string()
        );
    }

    #[test]
    fn override_address_and_port() {
        let config = parse_pool_config("para pool --address 127.0.0.1 --port 9999");
        let settings = default_settings();

        assert_eq!(config.address(&settings), "127.0.0.1");
        assert_eq!(config.port(&settings), 9999);
    }

    #[test]
    fn override_chain_via_global_flag() {
        let env = std::collections::BTreeMap::new();
        let opts = crate::options::Options::try_parse_from(["para", "--chain", "signet"]).unwrap();
        let settings = Settings::merge(opts, env).unwrap();
        let _config = parse_pool_config("para pool");
        assert_eq!(settings.chain(), Chain::Signet);
    }

    #[test]
    fn override_version_mask() {
        let config = parse_pool_config("para pool --version-mask 00fff000");
        let settings = default_settings();
        assert_eq!(
            config.version_mask(&settings),
            Version::from_str("00fff000").unwrap()
        );
    }

    #[test]
    fn start_diff() {
        let settings = default_settings();

        let config = parse_pool_config("para pool --start-diff 0.00001");
        assert_eq!(config.start_diff(&settings), Difficulty::from(0.00001));

        let config = parse_pool_config("para pool --start-diff 111");
        assert_eq!(config.start_diff(&settings), Difficulty::from(111.0));

        let config = parse_pool_config("para pool");
        assert_eq!(config.start_diff(&settings), Difficulty::from(1.0));
    }

    #[test]
    fn vardiff_period() {
        let settings = default_settings();

        let config = parse_pool_config("para pool --vardiff-period 10.0");
        assert_eq!(config.vardiff_period(&settings), Duration::from_secs(10));

        let config = parse_pool_config("para pool --vardiff-period 0.5");
        assert_eq!(config.vardiff_period(&settings), Duration::from_millis(500));

        let config = parse_pool_config("para pool");
        assert_eq!(config.vardiff_period(&settings), Duration::from_secs(5));
    }

    #[test]
    fn vardiff_window() {
        let settings = default_settings();

        let config = parse_pool_config("para pool --vardiff-window 60");
        assert_eq!(config.vardiff_window(&settings), Duration::from_secs(60));

        let config = parse_pool_config("para pool --vardiff-window 600.5");
        assert_eq!(
            config.vardiff_window(&settings),
            Duration::from_secs_f64(600.5)
        );

        let config = parse_pool_config("para pool");
        assert_eq!(config.vardiff_window(&settings), Duration::from_secs(300));
    }

    #[test]
    fn zmq_block_notifications() {
        let settings = default_settings();
        let config = parse_pool_config("para pool --zmq-block-notifications tcp://127.0.0.1:69");
        assert_eq!(
            config.zmq_block_notifications(&settings),
            "tcp://127.0.0.1:69".parse().unwrap()
        );
    }
}
