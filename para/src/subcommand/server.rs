use {super::*, error::ServerResult};

mod error;

#[derive(Clone, Debug, Parser)]
pub struct Server {
    #[clap(long, help = "Listen at <ADDRESS>")]
    pub(crate) address: Option<String>,
    #[arg(long, help = "Request ACME TLS certificate for <ACME_DOMAIN>")]
    pub(crate) acme_domain: Option<String>,
    #[arg(long, help = "Provide ACME contact <ACME_CONTACT>")]
    pub(crate) acme_contact: Option<String>,
    #[clap(long, help = "Listen on <PORT>")]
    pub(crate) port: Option<u16>,
}

impl Server {
    pub async fn run(&self, options: Options, handle: Handle) -> Result {
        let log_dir = options.log_dir();

        log::info!("Serving files in {}", log_dir.display());

        let database = Database::new(&options).await?;

        let router = Router::new()
            .nest_service("/pool/", ServeDir::new(log_dir.join("pool")))
            .nest_service("/users/", ServeDir::new(log_dir.join("users")))
            .layer(SetResponseHeaderLayer::overriding(
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain"),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                CONTENT_DISPOSITION,
                HeaderValue::from_static("inline"),
            ))
            .route("/splits", get(Self::get_splits))
            .route("/payouts/{blockheight}", get(Self::get_payouts))
            // TODO: RPC call to get total block output
            .route("/sat_split/{blockheight}", get(Self::get_sat_split))
            .layer(Extension(database));

        self.spawn(
            router,
            handle,
            self.address.clone(),
            self.port,
            options.data_dir(),
            self.acme_domain.clone(),
            self.acme_contact.clone(),
        )?
        .await??;

        Ok(())
    }

    pub(crate) async fn get_splits(
        Extension(database): Extension<Database>,
    ) -> ServerResult<Response> {
        Ok(Json(database.get_splits().await?).into_response())
    }

    pub(crate) async fn get_sat_split(
        Extension(database): Extension<Database>,
    ) -> ServerResult<Response> {
        Ok(Json("").into_response())
    }

    pub(crate) async fn get_payouts(
        Path(blockheight): Path<u32>,
        Extension(database): Extension<Database>,
    ) -> ServerResult<Response> {
        Ok(Json(
            database
                .get_payouts(blockheight.try_into().unwrap())
                .await?,
        )
        .into_response())
    }

    fn spawn(
        &self,
        router: Router,
        handle: Handle,
        address: Option<String>,
        port: Option<u16>,
        data_dir: PathBuf,
        acme_domain: Option<String>,
        acme_contact: Option<String>,
    ) -> Result<task::JoinHandle<io::Result<()>>> {
        let acme_cache = data_dir.join("acme-cache");

        let address = address.unwrap_or_else(|| "0.0.0.0".into());

        Ok(tokio::spawn(async move {
            match (acme_domain, acme_contact) {
                (Some(acme_domain), Some(acme_contact)) => {
                    log::info!(
                        "Getting certificate for {acme_domain} using contact email {acme_contact}"
                    );

                    let addr = (address, port.unwrap_or(443))
                        .to_socket_addrs()?
                        .next()
                        .unwrap();

                    log::info!("Listening on https://{addr}");

                    axum_server::Server::bind(addr)
                        .handle(handle)
                        .acceptor(Self::acceptor(acme_domain, acme_contact, acme_cache).unwrap())
                        .serve(router.into_make_service())
                        .await
                }
                _ => {
                    let addr = (address, port.unwrap_or(80))
                        .to_socket_addrs()?
                        .next()
                        .unwrap();

                    log::info!("Listening on http://{addr}");

                    axum_server::Server::bind(addr)
                        .handle(handle)
                        .serve(router.into_make_service())
                        .await
                }
            }
        }))
    }

    fn acceptor(
        acme_domain: String,
        acme_contact: String,
        acme_cache: PathBuf,
    ) -> Result<AxumAcceptor> {
        static RUSTLS_PROVIDER_INSTALLED: LazyLock<bool> = LazyLock::new(|| {
            rustls::crypto::ring::default_provider()
                .install_default()
                .is_ok()
        });

        let config = AcmeConfig::new(vec![acme_domain])
            .contact(vec![acme_contact])
            .cache_option(Some(DirCache::new(acme_cache)))
            .directory(if cfg!(test) {
                LETS_ENCRYPT_STAGING_DIRECTORY
            } else {
                LETS_ENCRYPT_PRODUCTION_DIRECTORY
            });

        let mut state = config.state();

        ensure! {
          *RUSTLS_PROVIDER_INSTALLED,
          "failed to install rustls ring crypto provider",
        }

        let mut server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(state.resolver());

        server_config.alpn_protocols = vec!["h2".into(), "http/1.1".into()];

        let acceptor = state.axum_acceptor(Arc::new(server_config));

        tokio::spawn(async move {
            while let Some(result) = state.next().await {
                match result {
                    Ok(ok) => log::info!("ACME event: {:?}", ok),
                    Err(err) => log::error!("ACME error: {:?}", err),
                }
            }
        });

        Ok(acceptor)
    }
}
