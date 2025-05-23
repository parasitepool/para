use {
    super::*,
    crate::templates::{PageContent, PageHtml, healthcheck::HealthcheckHtml, home::HomeHtml},
    error::{OptionExt, ServerError, ServerResult},
};

mod error;

#[derive(RustEmbed)]
#[folder = "static"]
struct StaticAssets;

#[derive(Serialize, Debug)]
pub(crate) struct HealthStatus {
    disk_usage_percent: f64,
    memory_usage_percent: f64,
    cpu_usage_percent: f64,
    uptime_seconds: u64,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct Payment {
    pub(crate) lightning_address: String,
    pub(crate) amount: i64,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct SatSplit {
    pub(crate) block_height: i32,
    pub(crate) block_hash: String,
    pub(crate) total_payment_amount: i64,
    pub(crate) payments: Vec<Payment>,
}

#[derive(Clone, Debug, Parser)]
pub struct Server {
    #[clap(long, help = "Listen at <ADDRESS>")]
    pub(crate) address: Option<String>,
    #[arg(long, help = "Request ACME TLS certificate for <ACME_DOMAIN>")]
    pub(crate) acme_domain: Vec<String>,
    #[arg(long, help = "Provide ACME contact <ACME_CONTACT>")]
    pub(crate) acme_contact: Vec<String>,
    #[clap(long, help = "Listen on <PORT>")]
    pub(crate) port: Option<u16>,
}

impl Server {
    pub async fn run(&self, options: Options, handle: Handle) -> Result {
        let log_dir = options.log_dir();

        log::info!("Serving files in {}", log_dir.display());

        let database = Database::new(&options).await?;

        let domain = self.domains()?.first().expect("should have domain").clone();

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
            .route("/", get(Self::home))
            .route("/healthcheck", get(Self::healthcheck))
            .route("/payouts/{blockheight}", get(Self::payouts))
            .route("/split", get(Self::open_split))
            .route("/split/{blockheight}", get(Self::sat_split))
            .route("/static/{*path}", get(Self::static_assets))
            .layer(Extension(domain))
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

    async fn home(Extension(domain): Extension<String>) -> ServerResult<PageHtml<HomeHtml>> {
        Ok(HomeHtml {
            stratum_url: format!("{}:42069", domain),
        }
        .page(domain))
    }

    pub(crate) async fn healthcheck(
        Extension(domain): Extension<String>,
    ) -> ServerResult<PageHtml<HealthcheckHtml>> {
        let health_status = tokio::task::spawn_blocking(|| {
            let mut system = System::new_all();
            system.refresh_all();

            let path = std::env::current_dir().map_err(|e| ServerError::Internal(e.into()))?;
            let mut disk_usage_percent = 0.0;
            let disks = Disks::new_with_refreshed_list();
            for disk in &disks {
                if path.starts_with(disk.mount_point()) {
                    let total = disk.total_space();
                    if total > 0 {
                        disk_usage_percent =
                            100.0 * (total - disk.available_space()) as f64 / total as f64;
                    }
                    break;
                }
            }

            let total_memory = system.total_memory();
            let memory_usage_percent = if total_memory > 0 {
                100.0 * system.used_memory() as f64 / total_memory as f64
            } else {
                -1.0
            };

            system.refresh_cpu_all();
            std::thread::sleep(std::time::Duration::from_millis(100));
            system.refresh_cpu_all();
            let cpu_usage_percent: f64 = system.global_cpu_usage().into();

            let uptime_seconds = System::uptime();

            Ok::<_, ServerError>(HealthStatus {
                disk_usage_percent,
                memory_usage_percent,
                cpu_usage_percent,
                uptime_seconds,
            })
        })
        .await
        .map_err(|e| ServerError::Internal(e.into()))??;

        Ok(HealthcheckHtml {
            disk_usage_percent: format!("{:.2}", health_status.disk_usage_percent),
            memory_usage_percent: format!("{:.2}", health_status.memory_usage_percent),
            cpu_usage_percent: format!("{:.2}", health_status.cpu_usage_percent),
            uptime_seconds: health_status.uptime_seconds,
        }
        .page(domain))
    }

    pub(crate) async fn payouts(
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

    pub(crate) async fn open_split(
        Extension(database): Extension<Database>,
    ) -> ServerResult<Response> {
        Ok(Json(database.get_split().await?).into_response())
    }

    pub(crate) async fn sat_split(
        Path(blockheight): Path<u32>,
        Extension(database): Extension<Database>,
    ) -> ServerResult<Response> {
        if blockheight == 0 {
            return Err(ServerError::NotFound("block not mined by parasite".into()));
        }

        let Some((blockheight, blockhash, coinbasevalue)) = database
            .get_total_coinbase(blockheight.try_into().unwrap())
            .await?
        else {
            return Err(ServerError::NotFound("block not mined by parasite".into()));
        };

        let total_payment_amount = coinbasevalue.saturating_sub(COIN_VALUE.try_into().unwrap());

        let payouts = database.get_payouts(blockheight).await?;

        let mut payments = Vec::new();
        for payout in payouts {
            if let Some(lnurl) = payout.lnurl {
                payments.push(Payment {
                    lightning_address: lnurl,
                    amount: (total_payment_amount / payout.total_shares) * payout.payable_shares,
                });
            }
        }

        Ok(Json(SatSplit {
            block_height: blockheight,
            block_hash: blockhash,
            total_payment_amount,
            payments,
        })
        .into_response())
    }

    pub(crate) async fn static_assets(Path(path): Path<String>) -> ServerResult<Response> {
        let content = StaticAssets::get(if let Some(stripped) = path.strip_prefix('/') {
            stripped
        } else {
            &path
        })
        .ok_or_not_found(|| format!("asset {path}"))?;

        let mime = mime_guess::from_path(path).first_or_octet_stream();

        Ok(Response::builder()
            .header(CONTENT_TYPE, mime.as_ref())
            .body(content.data.into())
            .unwrap())
    }

    fn domains(&self) -> Result<Vec<String>> {
        if !self.acme_domain.is_empty() {
            Ok(self.acme_domain.clone())
        } else {
            Ok(vec![
                System::host_name().ok_or(anyhow!("no hostname found"))?,
            ])
        }
    }

    fn spawn(
        &self,
        router: Router,
        handle: Handle,
        address: Option<String>,
        port: Option<u16>,
        data_dir: PathBuf,
        acme_domain: Vec<String>,
        acme_contact: Vec<String>,
    ) -> Result<task::JoinHandle<io::Result<()>>> {
        let acme_cache = data_dir.join("acme-cache");

        let address = address.unwrap_or_else(|| "0.0.0.0".into());

        Ok(tokio::spawn(async move {
            if !acme_domain.is_empty() && !acme_contact.is_empty() {
                log::info!(
                    "Getting certificate for {} using contact email {}",
                    acme_domain[0],
                    acme_contact[0]
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
            } else {
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
        }))
    }

    fn acceptor(
        acme_domain: Vec<String>,
        acme_contact: Vec<String>,
        acme_cache: PathBuf,
    ) -> Result<AxumAcceptor> {
        static RUSTLS_PROVIDER_INSTALLED: LazyLock<bool> = LazyLock::new(|| {
            rustls::crypto::ring::default_provider()
                .install_default()
                .is_ok()
        });

        let config = AcmeConfig::new(acme_domain)
            .contact(acme_contact)
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

#[cfg(test)]
mod test {
    #[test]
    fn validate_math() {
        let a: i64 = 3;
        let b: i64 = 2;
        assert_eq!(a / b, 1);
    }

    #[test]
    fn invalid_math() {
        let a: i64 = 3;
        let b: i64 = 2;
        assert!(a / b != 2);
    }
}
