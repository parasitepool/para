use super::*;

mod miner;
mod server;

#[derive(Debug, Parser)]
pub(crate) enum Subcommand {
    #[command(about = "Run a toy worker")]
    Miner(miner::Miner),
    #[command(about = "Run API server")]
    Server(server::Server),
}

impl Subcommand {
    pub(crate) fn run(self, options: Options) -> Result {
        match self {
            Self::Server(server) => {
                let handle = Handle::new();

                Runtime::new()?.block_on(async { server.run(options, handle).await.unwrap() });

                Ok(())
            }
            Self::Miner(miner) => Ok(miner.run()?),
        }
    }
}
