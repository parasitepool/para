use super::*;

mod server;
mod sync;

#[derive(Debug, Parser)]
pub(crate) enum Subcommand {
    #[command(about = "Run API server")]
    Server(server::Server),

    #[command(about = "Send shares to ZMQ endpoint")]
    SyncSend(sync::SyncSend),

    #[command(about = "Receive and process shares from ZMQ endpoint")]
    SyncReceive(sync::SyncReceive),
}

impl Subcommand {
    pub(crate) fn run(self, options: Options) -> Result {
        match self {
            Self::Server(server) => {
                let handle = Handle::new();

                Runtime::new()?.block_on(async { server.run(options, handle).await.unwrap() });

                Ok(())
            }
            Self::SyncSend(sync_send) => {
                let handle = Handle::new();

                Runtime::new()?.block_on(async { sync_send.run(options, handle).await.unwrap() });

                Ok(())
            }
            Self::SyncReceive(sync_receive) => {
                let handle = Handle::new();

                Runtime::new()?
                    .block_on(async { sync_receive.run(options, handle).await.unwrap() });

                Ok(())
            }
        }
    }
}
