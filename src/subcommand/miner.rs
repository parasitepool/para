use {super::*, controller::Controller, hasher::Hasher, stratum::Client};

mod controller;
mod hasher;

#[derive(Debug, Parser)]
pub(crate) struct Miner {
    #[arg(long, help = "Stratum <HOST>")]
    host: String,
    #[arg(long, help = "Stratum <PORT>")]
    port: u16,
    #[arg(long, help = "Stratum <USERNAME>")]
    username: String,
    #[arg(long, help = "Stratum <PASSWORD>")]
    password: String,
    // add flag to exit on share submission or block found
}

impl Miner {
    pub(crate) fn run(&self) -> Result {
        Runtime::new()?.block_on(async {
            let client =
                Client::connect(&self.host, self.port, &self.username, &self.password).await?;

            let controller = Controller::new(client).await?;

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
            "para miner \
                --host parasite.wtf \
                --port 42069 \
                --username bc1q8jx6g9ujlqmdx3jnt3ap6ll2fdwqjdkdgs959m.worker1.aed48ef@parasite.sati.pro \
                --password x",
        );
    }
}
