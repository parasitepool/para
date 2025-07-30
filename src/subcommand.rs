use super::*;

mod miner;
mod ping;
pub(crate) mod server;
mod sync;

#[derive(Debug, Parser)]
pub(crate) enum Subcommand {
    #[command(about = "Run a toy miner")]
    Miner(miner::Miner),
    #[command(about = "Measure Stratum message ping")]
    Ping(ping::Ping),
    #[command(about = "Run API server")]
    Server(server::Server),
    #[command(about = "Send shares to ZMQ endpoint")]
    SyncSend(sync::SyncSend),
    #[command(about = "Receive and process shares from ZMQ endpoint")]
    SyncReceive(sync::SyncReceive),
}

impl Subcommand {
    pub(crate) fn run(self) -> Result {
        match self {
            Self::Miner(miner) => miner.run(),
            Self::Ping(ping) => Runtime::new()?.block_on(async { ping.run().await }),
            Self::Server(server) => {
                let handle = Handle::new();
                Runtime::new()?.block_on(async { server.run(handle).await })
            }
            Self::SyncSend(sync_send) => {
                let handle = Handle::new();

                Runtime::new()?.block_on(async { sync_send.run(handle).await.unwrap() });

                Ok(())
            }
            Self::SyncReceive(sync_receive) => {
                let handle = Handle::new();

                Runtime::new()?.block_on(async { sync_receive.run(handle).await.unwrap() });

                Ok(())
            }
        }
    }
}
