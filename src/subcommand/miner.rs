use {
    super::*,
    controller::Controller,
    hasher::Hasher,
    metrics::Metrics,
    settings::Settings,
    stratum::{Client, ClientConfig},
};

mod controller;
mod hasher;
mod metrics;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Mode {
    Continuous,
    ShareFound,
    BlockFound,
}

#[derive(Debug, Parser)]
pub(crate) struct Miner {
    #[arg(help = "Stratum <HOST:PORT>.")]
    stratum_endpoint: Option<String>,
    #[arg(long, help = "Stratum <USERNAME>.")]
    username: Option<String>,
    #[arg(long, help = "Stratum <PASSWORD>.")]
    password: Option<String>,
    #[arg(
        long,
        value_enum,
        help = "Mining mode: <continuous|share-found|block-found>."
    )]
    mode: Option<Mode>,
    #[arg(long, help = "Number of <CPU_CORES> to use.")]
    cpu_cores: Option<usize>,
    #[arg(long, help = "Hash rate to <THROTTLE> to.")]
    throttle: Option<HashRate>,
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
    pub(crate) async fn run(self, settings: Settings, cancel_token: CancellationToken) -> Result {
        let stratum_endpoint = self
            .stratum_endpoint
            .or(settings.miner_stratum_endpoint.clone())
            .ok_or_else(|| anyhow!("stratum endpoint required"))?;

        let username = self
            .username
            .or(settings.miner_username.clone())
            .ok_or_else(|| anyhow!("username required"))?;

        let password = self.password.or(settings.miner_password.clone());

        let mode_str = self
            .mode
            .map(|m| match m {
                Mode::Continuous => "continuous",
                Mode::ShareFound => "share-found",
                Mode::BlockFound => "block-found",
            })
            .or(settings.miner_mode.as_deref())
            .unwrap_or("continuous");

        let mode = match mode_str {
            "share-found" => Mode::ShareFound,
            "block-found" => Mode::BlockFound,
            _ => Mode::Continuous,
        };

        let cpu_cores = self.cpu_cores.or(settings.miner_cpu_cores);

        let throttle = self.throttle.or_else(|| {
            settings
                .miner_throttle
                .as_ref()
                .and_then(|s| s.parse::<HashRate>().ok())
        });

        let username: Username = username.into();

        info!("Connecting to {stratum_endpoint} with user {username}");

        let address = resolve_stratum_endpoint(&stratum_endpoint).await?;

        let config = ClientConfig {
            address: address.to_string(),
            username: username.clone(),
            user_agent: USER_AGENT.into(),
            password,
            timeout: Duration::from_secs(10),
        };

        let client = Client::new(config);

        let mut system = System::new();
        system.refresh_cpu_all();
        let available_cpu_cores = system.cpus().len();

        let cpu_cores = if let Some(cpu_cores) = cpu_cores {
            std::cmp::min(cpu_cores, available_cpu_cores)
        } else {
            available_cpu_cores
        };

        info!("Available CPU cores: {}", available_cpu_cores);
        info!("CPU cores to use: {}", cpu_cores);

        let shares =
            Controller::run(client, username, cpu_cores, throttle, mode, cancel_token).await?;

        println!("{}", serde_json::to_string_pretty(&shares)?);

        Ok(())
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
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
                --username bc1q8jx6g9ujlqmdx3jnt3ap6ll2fdwqjdkdgs959m.worker1.aed48ef@parasite.sati.pro \
                --password x",
        );
        assert_eq!(miner.stratum_endpoint, Some("parasite.wtf:42069".into()));
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

        assert!(miner.mode.is_none());
    }

    #[test]
    fn parse_args_with_mode_share_found() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --password x \
            --mode share-found",
        );

        assert!(matches!(miner.mode, Some(Mode::ShareFound)));
    }

    #[test]
    fn parse_args_with_mode_block_found() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
            --username test.worker \
            --password x \
            --mode block-found",
        );

        assert!(matches!(miner.mode, Some(Mode::BlockFound)));
    }
}
