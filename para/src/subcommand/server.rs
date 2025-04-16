use super::*;

#[derive(Clone, Debug, Parser)]
pub struct Server {
    #[clap(long, help = "Listen at <ADDRESS>")]
    pub(crate) address: Option<String>,
    #[clap(long, help = "Listen on <PORT>")]
    pub(crate) port: Option<u16>,
}

impl Server {
    pub async fn run(&self, options: Options, handle: Handle) -> Result {
        let log_dir = options.log_dir();

        log::info!("Serving files in {}", log_dir.display());

        let mut router = Router::new()
            .nest_service("/pool/", ServeDir::new(log_dir.join("pool")))
            .nest_service("/users/", ServeDir::new(log_dir.join("users")));

        router = router.layer(ServiceBuilder::new()
                .layer(SetResponseHeaderLayer::overriding(
                    CONTENT_TYPE,
                    HeaderValue::from_static("text/plain"),
                ))
                .layer(SetResponseHeaderLayer::if_not_present(
                    CONTENT_DISPOSITION,
                    HeaderValue::from_static("inline"),
                )));

        self.spawn(router, handle, self.address.clone(), self.port)?
            .await??;

        Ok(())
    }

    fn spawn(
        &self,
        router: Router,
        handle: Handle,
        address: Option<String>,
        port: Option<u16>,
    ) -> Result<task::JoinHandle<io::Result<()>>> {
        let address = match address {
            Some(address) => address,
            None => "0.0.0.0".into(),
        };

        let addr = (address, port.unwrap_or(80))
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow!("failed to get socket addrs"))?;

        Ok(tokio::spawn(async move {
            axum_server::Server::bind(addr)
                .handle(handle)
                .serve(router.into_make_service())
                .await
        }))
    }
}
