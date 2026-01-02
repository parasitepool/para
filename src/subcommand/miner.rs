use {
    super::*,
    controller::{Controller, VersionRollingConfig},
    hasher::Hasher,
    metrics::Metrics,
    stratum::{Client, ClientConfig},
};

mod controller;
mod hasher;
mod metrics;
mod version_rolling;

// Re-export for external use (also makes them available internally)
pub use version_rolling::{
    BIP320_VERSION_MASK, MIN_VERSION_BITS, VersionRoller, apply_version_bits, extract_version_bits,
};

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Mode {
    Continuous,
    ShareFound,
    BlockFound,
}

#[derive(Debug, Parser)]
pub(crate) struct Miner {
    #[arg(help = "Stratum <HOST:PORT>.")]
    stratum_endpoint: String,
    #[arg(long, help = "Stratum <USERNAME>.")]
    username: Username,
    #[arg(long, help = "Stratum <PASSWORD>.")]
    password: Option<String>,
    #[arg(
        long,
        value_enum,
        default_value = "continuous",
        help = "Mining mode: <continuous|share-found|block-found>."
    )]
    mode: Mode,
    #[arg(long, help = "Number of <CPU_CORES> to use.")]
    cpu_cores: Option<usize>,
    #[arg(long, help = "Hash rate to <THROTTLE> to.")]
    throttle: Option<HashRate>,
    #[arg(long, help = "Disable version rolling (ASICBoost).")]
    no_version_rolling: bool,
    #[arg(
        long,
        help = "Custom version rolling mask in hex (default: BIP320 0x1FFFE000)."
    )]
    version_mask: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Share {
    pub extranonce1: Extranonce,
    pub extranonce2: Extranonce,
    pub job_id: JobId,
    pub nonce: Nonce,
    pub ntime: Ntime,
    pub username: Username,
    pub version_bits: Option<Version>,
}

impl Miner {
    pub(crate) async fn run(&self, cancel_token: CancellationToken) -> Result {
        info!(
            "Connecting to {} with user {}",
            self.stratum_endpoint, self.username
        );

        let address = resolve_stratum_endpoint(&self.stratum_endpoint)
            .await
            .with_context(|| {
                format!(
                    "failed to resolve stratum endpoint `{}`",
                    self.stratum_endpoint
                )
            })?;

        let config = ClientConfig {
            address: address.to_string(),
            username: self.username.clone(),
            user_agent: USER_AGENT.into(),
            password: self.password.clone(),
            timeout: Duration::from_secs(10),
        };

        let client = Client::new(config);

        let mut system = System::new();
        system.refresh_cpu_all();
        let available_cpu_cores = system.cpus().len();

        let cpu_cores = if let Some(cpu_cores) = self.cpu_cores {
            std::cmp::min(cpu_cores, available_cpu_cores)
        } else {
            available_cpu_cores
        };

        info!("Available CPU cores: {}", available_cpu_cores);
        info!("CPU cores to use: {}", cpu_cores);

        // Parse version rolling configuration
        let version_rolling = self.parse_version_rolling_config()?;

        let shares = Controller::run_with_version_rolling(
            client,
            self.username.clone(),
            cpu_cores,
            self.throttle,
            self.mode,
            cancel_token,
            version_rolling,
        )
        .await?;

        println!("{}", serde_json::to_string_pretty(&shares)?);

        Ok(())
    }

    fn parse_version_rolling_config(&self) -> Result<VersionRollingConfig> {
        if self.no_version_rolling {
            info!("Version rolling disabled via CLI");
            return Ok(VersionRollingConfig::disabled());
        }

        let mask = if let Some(mask_str) = &self.version_mask {
            let mask_str = mask_str.trim_start_matches("0x").trim_start_matches("0X");
            let mask = u32::from_str_radix(mask_str, 16)
                .map_err(|e| anyhow::anyhow!("Invalid version mask: {e}"))?;

            if !VersionRoller::validate_mask(mask) {
                warn!(
                    "Version mask {:#x} has fewer than {} bits, mining may be inefficient",
                    mask, MIN_VERSION_BITS
                );
            }

            mask
        } else {
            BIP320_VERSION_MASK
        };

        Ok(VersionRollingConfig::with_mask(mask))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_miner_args(args: &str) -> Miner {
        match Arguments::try_parse_from(args.split_whitespace()) {
            Ok(arguments) => match arguments.subcommand {
                Subcommand::Miner(miner) => miner,
                subcommand => panic!("unexpected subcommand: {subcommand:?}"),
            },
            Err(err) => panic!("error parsing arguments: {err}"),
        }
    }

    #[test]
    fn parse_args() {
        parse_miner_args(
            "para miner parasite.wtf:42069 \
                --username bc1q8jx6g9ujlqmdx3jnt3ap6ll2fdwqjdkdgs959m.worker1.aed48ef@parasite.sati.pro \
                --password x",
        );
    }

    #[test]
    fn parse_args_with_cpu_cores() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
                --username test.worker \
                --password x \
                --cpu-cores 8
            ",
        );

        assert_eq!(miner.cpu_cores, Some(8));
    }

    #[test]
    fn parse_args_with_default_mode() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --password x",
        );

        assert!(matches!(miner.mode, Mode::Continuous));
    }

    #[test]
    fn parse_args_with_mode_share_found() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --password x \
            --mode share-found",
        );

        assert!(matches!(miner.mode, Mode::ShareFound));
    }

    #[test]
    fn parse_args_with_mode_block_found() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --password x \
            --mode block-found",
        );

        assert!(matches!(miner.mode, Mode::BlockFound));
    }

    // ==================== Version Rolling CLI Tests ====================

    #[test]
    fn parse_args_version_rolling_enabled_by_default() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --password x",
        );

        assert!(!miner.no_version_rolling);
        assert!(miner.version_mask.is_none());
    }

    #[test]
    fn parse_args_version_rolling_disabled() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --password x \
            --no-version-rolling",
        );

        assert!(miner.no_version_rolling);
    }

    #[test]
    fn parse_args_custom_version_mask() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --password x \
            --version-mask 0x1FFF0000",
        );

        assert!(!miner.no_version_rolling);
        assert_eq!(miner.version_mask, Some("0x1FFF0000".to_string()));
    }

    #[test]
    fn parse_args_custom_version_mask_without_prefix() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --password x \
            --version-mask 1FFFE000",
        );

        assert_eq!(miner.version_mask, Some("1FFFE000".to_string()));
    }

    #[test]
    fn parse_version_rolling_config_default() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker",
        );

        let config = miner.parse_version_rolling_config().unwrap();
        assert!(config.enabled);
        assert_eq!(config.mask, BIP320_VERSION_MASK);
    }

    #[test]
    fn parse_version_rolling_config_disabled() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --no-version-rolling",
        );

        let config = miner.parse_version_rolling_config().unwrap();
        assert!(!config.enabled);
        assert_eq!(config.mask, 0);
    }

    #[test]
    fn parse_version_rolling_config_custom_mask() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --version-mask 0xFF000000",
        );

        let config = miner.parse_version_rolling_config().unwrap();
        assert!(config.enabled);
        assert_eq!(config.mask, 0xFF000000);
    }

    #[test]
    fn parse_version_rolling_config_custom_mask_lowercase() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --version-mask 0xabcdef00",
        );

        let config = miner.parse_version_rolling_config().unwrap();
        assert_eq!(config.mask, 0xABCDEF00);
    }
}
