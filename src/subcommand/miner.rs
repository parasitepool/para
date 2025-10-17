use {super::*, controller::Controller, hasher::Hasher, stratum::Client};

mod controller;
mod hasher;

#[derive(Debug, Parser)]
pub(crate) struct Miner {
    #[arg(help = "Stratum <HOST:PORT>.")]
    stratum_endpoint: String,
    #[arg(long, help = "Stratum <USERNAME>.")]
    username: String,
    #[arg(long, help = "Stratum <PASSWORD>.")]
    password: Option<String>,
    #[arg(long, help = "Exit <ONCE> a share is found.")]
    once: bool,
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
    pub(crate) fn run(&self) -> Result {
        Runtime::new()?.block_on(async {
            info!(
                "Connecting to {} with user {}",
                self.stratum_endpoint, self.username
            );

            let address = resolve_stratum_endpoint(&self.stratum_endpoint).await?;

            let client = Client::connect(
                address,
                self.username.clone(),
                self.password.clone(),
                Duration::from_secs(5),
            )
            .await?;

            let controller = Controller::new(client, self.once).await?;

            let shares = controller.run().await?;

            println!("{}", serde_json::to_string_pretty(&shares)?);

            Ok(())
        })
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
}
