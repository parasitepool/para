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

            controller.run().await
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
