use super::*;

mod server;
mod worker;

#[derive(Debug, Parser)]
pub(crate) enum Subcommand {
    #[command(about = "Run API server")]
    Server(server::Server),
    #[command(about = "Run a toy worker")]
    Worker(worker::Worker),
}

impl Subcommand {
    pub(crate) fn run(self, options: Options) -> Result {
        match self {
            Self::Server(server) => {
                let handle = Handle::new();

                Runtime::new()?.block_on(async { server.run(options, handle).await.unwrap() });

                Ok(())
            }
            Self::Worker(worker) => worker.run(options)?,
        }
    }
}
