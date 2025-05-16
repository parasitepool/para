use super::*;

mod proxy;
mod server;

#[derive(Debug, Parser)]
pub(crate) enum Subcommand {
    #[command(about = "Run proxy server")]
    Proxy(proxy::Proxy),
    #[command(about = "Run API server")]
    Server(server::Server),
}

impl Subcommand {
    pub(crate) fn run(self, options: Options) -> Result {
        match self {
            Self::Proxy(proxy) => {
                let handle = Handle::new();

                Runtime::new()?.block_on(async { proxy.run(handle).await.unwrap() });

                Ok(())
            }
            Self::Server(server) => {
                let handle = Handle::new();

                Runtime::new()?.block_on(async { server.run(options, handle).await.unwrap() });

                Ok(())
            }
        }
    }
}
