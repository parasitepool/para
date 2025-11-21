use {
    super::*,
    controller::Controller,
    hasher::Hasher,
    metrics::{Metrics, spawn_throbber},
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
    stratum_endpoint: String,
    #[arg(long, help = "Stratum <USERNAME>.")]
    username: String,
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
    throttle: Option<ckpool::HashRate>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Share {
    pub extranonce1: Extranonce,
    pub extranonce2: Extranonce,
    pub job_id: JobId,
    pub nonce: Nonce,
    pub ntime: Ntime,
    pub username: String,
    pub version_bits: Option<Version>,
}

impl Miner {
    pub(crate) async fn run(&self, cancel_token: CancellationToken) -> Result {
        info!(
            "Connecting to {} with user {}",
            self.stratum_endpoint, self.username
        );

        let address = resolve_stratum_endpoint(&self.stratum_endpoint).await?;

        let config = ClientConfig {
            address: address.to_string(),
            username: self.username.clone(),
            user_agent: USER_AGENT.into(),
            password: self.password.clone(),
            timeout: Duration::from_secs(10),
        };

        let mut client = Client::new(config);
        client.connect().await?;

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

        let (controller, events) = Controller::new(
            client,
            self.username.clone(),
            cpu_cores,
            self.throttle,
            self.mode,
        )
        .await?;

        let shares = controller.run(events, cancel_token).await?;

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
}
