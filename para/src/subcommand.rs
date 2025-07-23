use super::*;

mod miner;
mod ping;
mod server;

#[derive(Debug, Parser)]
pub(crate) enum Subcommand {
    #[command(about = "Run a toy miner")]
    Miner(miner::Miner),
    #[command(about = "Measure Stratum message ping")]
    Ping(ping::Ping),
    #[command(about = "Run API server")]
    Server(server::Server),
}

impl Subcommand {
    pub(crate) fn run(self) -> Result {
        match self {
            Self::Miner(miner) => miner.run(),
            Self::Ping(ping) => {
                let handle = Handle::new();
                Runtime::new()?.block_on(async { ping.run(handle).await })
            }
            Self::Server(server) => {
                let handle = Handle::new();
                Runtime::new()?.block_on(async { server.run(handle).await })
            }
        }
    }
}
