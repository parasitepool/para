use {
    super::*,
    crate::subcommand::sync::{ShareBatch, SyncResponse},
    accept_json::AcceptJson,
    aggregator::Aggregator,
    axum::extract::{Path, Query},
    database::Database,
    error::{OptionExt, ServerError, ServerResult},
    server_config::ServerConfig,
    templates::{
        PageContent, PageHtml, dashboard::DashboardHtml, healthcheck::HealthcheckHtml,
        home::HomeHtml,
    },
};

mod accept_json;
mod aggregator;
pub mod api;
pub mod database;
mod error;
pub mod notifications;
mod server_config;
mod templates;

#[derive(RustEmbed)]
#[folder = "static"]
struct StaticAssets;

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

fn format_uptime(uptime_seconds: u64) -> String {
    let days = uptime_seconds / 86400;
    let hours = (uptime_seconds % 86400) / 3600;
    let minutes = (uptime_seconds % 3600) / 60;

    let plural = |n: u64, singular: &str| {
        if n == 1 {
            singular.to_string()
        } else {
            format!("{singular}s")
        }
    };

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{} {}", days, plural(days, "day")));
    }
    if hours > 0 {
        parts.push(format!("{} {}", hours, plural(hours, "hour")));
    }
    if minutes > 0 || parts.is_empty() {
        parts.push(format!("{} {}", minutes, plural(minutes, "minute")));
    }

    parts.join(", ")
}

fn exclusion_list_from_params(params: HashMap<String, String>) -> Vec<String> {
    params
        .get("excluded")
        .map(|s| s.split(',').map(String::from).collect::<Vec<_>>())
        .unwrap_or_default()
}

#[derive(Clone, Debug, Parser)]
pub struct Server {
    #[command(flatten)]
    pub(crate) config: ServerConfig,
}

impl Server {
    pub async fn run(&self, handle: Handle) -> Result {
        let config = Arc::new(self.config.clone());
        let log_dir = config.log_dir();
        let pool_dir = log_dir.join("pool");
        let user_dir = log_dir.join("users");

        if !pool_dir.exists() {
            warn!("Pool dir {} does not exist", pool_dir.display());
        }

        if !user_dir.exists() {
            warn!("User dir {} does not exist", user_dir.display());
        }

        let mut router = Router::new()
            .nest_service("/pool/", ServeDir::new(pool_dir))
            .route("/users", get(Self::users))
            .nest_service("/users/", ServeDir::new(user_dir))
            .layer(SetResponseHeaderLayer::overriding(
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain"),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                CONTENT_DISPOSITION,
                HeaderValue::from_static("inline"),
            ))
            .route("/", get(Self::home))
            .route("/healthcheck", self.with_auth(get(Self::healthcheck)))
            .route("/static/{*path}", get(Self::static_assets));

        match Database::new(config.database_url()).await {
            Ok(database) => {
                router = router
                    .route("/payouts/{blockheight}", get(Self::payouts))
                    .route(
                        "/payouts/range/{start_height}/{end_height}",
                        get(Self::payouts_range),
                    )
                    .route(
                        "/payouts/range/{start_height}/{end_height}/user/{username}",
                        get(Self::user_payout_range),
                    )
                    .route("/split", get(Self::open_split))
                    .route("/split/{blockheight}", get(Self::sat_split))
                    .route(
                        "/sync/batch",
                        self.with_auth(
                            post(Self::sync_batch)
                                .layer(DefaultBodyLimit::max(52428800 /* 50MB */)),
                        ),
                    )
                    .layer(Extension(database));
            }
            Err(err) => {
                warn!("Failed to connect to PostgreSQL: {err}",);
            }
        }

        router = router.layer(Extension(config.clone()));

        if !config.nodes().is_empty() {
            let aggregator = Aggregator::init(config.clone())?;
            router = router.merge(aggregator);
        } else {
            warn!("No aggregator nodes configured: skipping aggregator routes.");
        }

        info!("Serving files in {}", log_dir.display());

        self.spawn(config, router, handle)?.await??;

        Ok(())
    }

    fn with_auth<S>(&self, method_router: MethodRouter<S>) -> MethodRouter<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        if let Some((username, password)) = self.config.credentials() {
            method_router.layer(ValidateRequestHeaderLayer::basic(username, password))
        } else {
            method_router
        }
    }

    async fn home(
        Extension(config): Extension<Arc<ServerConfig>>,
    ) -> ServerResult<PageHtml<HomeHtml>> {
        let domain = config.domain();

        Ok(HomeHtml {
            stratum_url: format!("{domain}:42069"),
        }
        .page(domain))
    }

    async fn users(Extension(config): Extension<Arc<ServerConfig>>) -> ServerResult<Response> {
        task::block_in_place(|| {
            Ok(Json(
                fs::read_dir(config.log_dir().join("users"))
                    .map_err(|err| anyhow!(err))?
                    .filter_map(Result::ok)
                    .filter_map(|entry| entry.file_name().to_str().map(|s| s.to_string()))
                    .collect::<Vec<String>>(),
            )
            .into_response())
        })
    }

    pub(crate) async fn healthcheck(
        Extension(config): Extension<Arc<ServerConfig>>,
        AcceptJson(accept_json): AcceptJson,
    ) -> ServerResult<Response> {
        task::block_in_place(|| {
            let mut system = System::new_all();
            system.refresh_all();

            let path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

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
            let cpu_usage_percent: f64 = system.global_cpu_usage().into();

            let healthcheck = HealthcheckHtml {
                disk_usage_percent,
                memory_usage_percent,
                cpu_usage_percent,
                uptime: System::uptime(),
            };

            Ok(if accept_json {
                Json(healthcheck).into_response()
            } else {
                healthcheck.page(config.domain()).into_response()
            })
        })
    }

    pub(crate) async fn payouts(
        Path(blockheight): Path<u32>,
        Extension(database): Extension<Database>,
    ) -> ServerResult<Response> {
        Ok(Json(
            database
                .get_payouts(blockheight.try_into().unwrap(), "no filter address".into())
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

        let Some((blockheight, blockhash, coinbasevalue, _, username)) = database
            .get_total_coinbase(blockheight.try_into().unwrap())
            .await?
        else {
            return Err(ServerError::NotFound("block not mined by parasite".into()));
        };

        let total_payment_amount = coinbasevalue.saturating_sub(COIN_VALUE.try_into().unwrap());

        let payouts = database.get_payouts(blockheight, username).await?;

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

    pub(crate) async fn payouts_range(
        Path((start_height, end_height)): Path<(u32, u32)>,
        Query(params): Query<HashMap<String, String>>,
        Extension(database): Extension<Database>,
    ) -> ServerResult<Response> {
        let excluded_usernames = exclusion_list_from_params(params);

        Ok(Json(
            database
                .get_payouts_range(
                    start_height.try_into().unwrap(),
                    end_height.try_into().unwrap(),
                    excluded_usernames,
                )
                .await?,
        )
        .into_response())
    }

    pub(crate) async fn user_payout_range(
        Path((start_height, end_height, username)): Path<(u32, u32, String)>,
        Query(params): Query<HashMap<String, String>>,
        Extension(database): Extension<Database>,
    ) -> ServerResult<Response> {
        let excluded_usernames = exclusion_list_from_params(params);

        Ok(Json(
            database
                .get_user_payout_range(
                    start_height.try_into().unwrap(),
                    end_height.try_into().unwrap(),
                    username,
                    excluded_usernames,
                )
                .await?,
        )
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

    fn spawn(
        &self,
        config: Arc<ServerConfig>,
        router: Router,
        handle: Handle,
    ) -> Result<task::JoinHandle<io::Result<()>>> {
        let acme_cache = config.acme_cache();
        let acme_domains = config.domains()?;
        let acme_contacts = config.acme_contacts();
        let address = config.address();

        Ok(tokio::spawn(async move {
            if !acme_domains.is_empty() && !acme_contacts.is_empty() {
                info!(
                    "Getting certificate for {} using contact email {}",
                    acme_domains[0], acme_contacts[0]
                );

                let addr = (address, config.port().unwrap_or(443))
                    .to_socket_addrs()?
                    .next()
                    .unwrap();

                info!("Listening on https://{addr}");

                axum_server::Server::bind(addr)
                    .handle(handle)
                    .acceptor(Self::acceptor(acme_domains, acme_contacts, acme_cache).unwrap())
                    .serve(router.into_make_service())
                    .await
            } else {
                let addr = (address, config.port().unwrap_or(80))
                    .to_socket_addrs()?
                    .next()
                    .unwrap();

                info!("Listening on http://{addr}");

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
                    Ok(ok) => info!("ACME event: {:?}", ok),
                    Err(err) => error!("ACME error: {:?}", err),
                }
            }
        });

        Ok(acceptor)
    }

    pub(crate) async fn sync_batch(
        Extension(database): Extension<Database>,
        Extension(config): Extension<Arc<ServerConfig>>,
        Json(batch): Json<ShareBatch>,
    ) -> Result<Json<SyncResponse>, StatusCode> {
        info!(
            "Received sync batch {} with {} shares from {}",
            batch.batch_id,
            batch.shares.len(),
            batch.hostname
        );

        if let Some(block) = &batch.block {
            match database.upsert_block(block).await {
                Ok(_) => {
                    info!(
                        "Successfully upserted block for height {}",
                        block.blockheight
                    );

                    let notification_result = notifications::notify_block_found(
                        &config.alerts_ntfy_channel,
                        block.blockheight,
                        block.blockhash.clone(),
                        block.coinbasevalue.unwrap_or(0),
                        block
                            .username
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string()),
                    )
                    .await;

                    match notification_result {
                        Ok(_) => info!("Block notification sent successfully"),
                        Err(e) => error!("Failed to send block notification: {}", e),
                    }
                }
                Err(e) => error!("Warning: Failed to upsert block: {}", e),
            }
        }

        match Self::process_share_batch(&batch, &database).await {
            Ok(_) => {
                let response = SyncResponse {
                    batch_id: batch.batch_id,
                    received_count: batch.shares.len(),
                    status: "OK".to_string(),
                    error_message: None,
                };
                info!("Successfully processed batch {}", batch.batch_id);
                Ok(Json(response))
            }
            Err(e) => {
                let response = SyncResponse {
                    batch_id: batch.batch_id,
                    received_count: 0,
                    status: "ERROR".to_string(),
                    error_message: Some(e.to_string()),
                };
                error!("Failed to process batch {}: {}", batch.batch_id, e);
                Ok(Json(response))
            }
        }
    }

    async fn process_share_batch(batch: &ShareBatch, database: &Database) -> Result<()> {
        info!(
            "Processing {} shares from batch {}",
            batch.shares.len(),
            batch.batch_id
        );

        if batch.shares.is_empty() {
            return Ok(());
        }

        const MAX_SHARES_PER_SUBBATCH: usize = 2500;
        let mut tx = database
            .pool
            .begin()
            .await
            .map_err(|e| anyhow!("Failed to start transaction: {e}"))?;

        for (chunk_idx, chunk) in batch.shares.chunks(MAX_SHARES_PER_SUBBATCH).enumerate() {
            info!(
                "Processing sub-batch {}/{} with {} shares",
                chunk_idx + 1,
                batch.shares.len().div_ceil(MAX_SHARES_PER_SUBBATCH),
                chunk.len()
            );

            let mut query_builder = sqlx::QueryBuilder::new(
                "INSERT INTO remote_shares (
                id, origin, blockheight, workinfoid, clientid, enonce1, nonce2, nonce, ntime,
                diff, sdiff, hash, result, reject_reason, error, errn, createdate, createby,
                createcode, createinet, workername, username, lnurl, address, agent
            ) ",
            );

            query_builder.push_values(chunk, |mut b, share| {
                b.push_bind(share.id)
                    .push_bind(&batch.hostname)
                    .push_bind(share.blockheight)
                    .push_bind(share.workinfoid)
                    .push_bind(share.clientid)
                    .push_bind(&share.enonce1)
                    .push_bind(&share.nonce2)
                    .push_bind(&share.nonce)
                    .push_bind(&share.ntime)
                    .push_bind(share.diff)
                    .push_bind(share.sdiff)
                    .push_bind(&share.hash)
                    .push_bind(share.result)
                    .push_bind(&share.reject_reason)
                    .push_bind(&share.error)
                    .push_bind(share.errn)
                    .push_bind(&share.createdate)
                    .push_bind(&share.createby)
                    .push_bind(&share.createcode)
                    .push_bind(&share.createinet)
                    .push_bind(&share.workername)
                    .push_bind(&share.username)
                    .push_bind(&share.lnurl)
                    .push_bind(&share.address)
                    .push_bind(&share.agent);
            });

            query_builder.push(
                " ON CONFLICT (id, origin) DO UPDATE SET
                blockheight = EXCLUDED.blockheight,
                workinfoid = EXCLUDED.workinfoid,
                clientid = EXCLUDED.clientid,
                enonce1 = EXCLUDED.enonce1,
                nonce2 = EXCLUDED.nonce2,
                nonce = EXCLUDED.nonce,
                ntime = EXCLUDED.ntime,
                diff = EXCLUDED.diff,
                sdiff = EXCLUDED.sdiff,
                hash = EXCLUDED.hash,
                result = EXCLUDED.result,
                reject_reason = EXCLUDED.reject_reason,
                error = EXCLUDED.error,
                errn = EXCLUDED.errn,
                createdate = EXCLUDED.createdate,
                createby = EXCLUDED.createby,
                createcode = EXCLUDED.createcode,
                createinet = EXCLUDED.createinet,
                workername = EXCLUDED.workername,
                username = EXCLUDED.username,
                lnurl = EXCLUDED.lnurl,
                address = EXCLUDED.address,
                agent = EXCLUDED.agent",
            );

            let query = query_builder.build();
            query.execute(&mut *tx).await.map_err(|e| {
                anyhow!(
                    "Failed to batch insert shares in sub-batch {}: {e}",
                    chunk_idx + 1
                )
            })?;
        }

        tx.commit()
            .await
            .map_err(|e| anyhow!("Failed to commit transaction: {e}"))?;

        let total_diff: f64 = batch.shares.iter().filter_map(|s| s.diff).sum();
        let worker_count = batch
            .shares
            .iter()
            .filter_map(|s| s.workername.as_ref())
            .collect::<std::collections::HashSet<_>>()
            .len();

        let min_blockheight = batch.shares.iter().filter_map(|s| s.blockheight).min();
        let max_blockheight = batch.shares.iter().filter_map(|s| s.blockheight).max();

        info!(
            "Stored batch {} with {} shares: total difficulty: {:.2}, {} unique workers, blockheights: {:?}-{:?}, origin: {}",
            batch.batch_id,
            batch.shares.len(),
            total_diff,
            worker_count,
            min_blockheight,
            max_blockheight,
            batch.hostname
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_server_config(args: &str) -> ServerConfig {
        match Arguments::try_parse_from(args.split_whitespace()) {
            Ok(arguments) => match arguments.subcommand {
                Subcommand::Server(server) => server.config,
                subcommand => panic!("unexpected subcommand: {subcommand:?}"),
            },
            Err(err) => panic!("error parsing arguments: {err}"),
        }
    }

    #[test]
    fn default_address() {
        let config = parse_server_config("para server");
        assert_eq!(config.address(), "0.0.0.0");
    }

    #[test]
    fn override_address() {
        let config = parse_server_config("para server --address 127.0.0.1");
        assert_eq!(config.address(), "127.0.0.1");
    }

    #[test]
    fn default_acme_cache() {
        let config = parse_server_config("para server");
        assert_eq!(config.acme_cache(), PathBuf::from("acme-cache"));
    }

    #[test]
    fn override_acme_cache_via_data_dir() {
        let config = parse_server_config("para server --data-dir /custom/path");
        assert_eq!(
            config.acme_cache(),
            PathBuf::from("/custom/path/acme-cache")
        );
    }

    #[test]
    fn override_acme_domains() {
        let config =
            parse_server_config("para server --acme-domain example.com --acme-domain foo.bar");
        assert_eq!(
            config.domains().unwrap(),
            vec!["example.com".to_string(), "foo.bar".to_string()]
        );
    }

    #[test]
    fn default_acme_contacts() {
        let config = parse_server_config("para server");
        assert!(config.acme_contacts().is_empty());
    }

    #[test]
    fn override_acme_contacts() {
        let config = parse_server_config("para server --acme-contact admin@example.com");
        assert_eq!(
            config.acme_contacts(),
            vec!["admin@example.com".to_string()]
        );
    }

    #[test]
    fn default_credentials() {
        let config = parse_server_config("para server");
        assert_eq!(config.credentials(), None);
    }

    #[test]
    fn credentials_both_provided() {
        let config = parse_server_config("para server --username satoshi --password secret");
        assert_eq!(config.credentials(), Some(("satoshi", "secret")));
    }

    #[test]
    fn default_domain() {
        let config = parse_server_config("para server --acme-domain example.com");
        assert_eq!(config.domain(), "example.com");
    }

    #[test]
    fn default_domains_fallback() {
        let config = parse_server_config("para server");
        let domains = config.domains().unwrap();
        assert!(!domains.is_empty(), "Expected hostname fallback");
    }

    #[test]
    fn override_domains_no_fallback() {
        let config = parse_server_config("para server --acme-domain custom.domain");
        let domains = config.domains().unwrap();
        assert_eq!(domains, vec!["custom.domain".to_string()]);
    }

    #[test]
    fn default_data_dir() {
        let config = parse_server_config("para server");
        assert_eq!(config.data_dir(), PathBuf::new());
    }

    #[test]
    fn override_data_dir() {
        let config = parse_server_config("para server --data-dir /var/pool");
        assert_eq!(config.data_dir(), PathBuf::from("/var/pool"));
    }

    #[test]
    fn default_database_url() {
        let config = parse_server_config("para server");
        assert_eq!(
            config.database_url(),
            "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool"
        );
    }

    #[test]
    fn override_database_url() {
        let config = parse_server_config("para server --database-url postgres://user:pass@host/db");
        assert_eq!(config.database_url(), "postgres://user:pass@host/db");
    }

    #[test]
    fn default_log_dir() {
        let config = parse_server_config("para server");
        assert_eq!(config.log_dir(), std::env::current_dir().unwrap());
    }

    #[test]
    fn override_log_dir() {
        let config = parse_server_config("para server --log-dir /logs");
        assert_eq!(config.log_dir(), PathBuf::from("/logs"));
    }

    #[test]
    fn default_port() {
        let config = parse_server_config("para server");
        assert_eq!(config.port(), None);
    }

    #[test]
    fn override_port() {
        let config = parse_server_config("para server --port 8080");
        assert_eq!(config.port(), Some(8080));
    }

    #[test]
    #[should_panic(expected = "required")]
    fn credentials_only_username_panics() {
        parse_server_config("para server --username satoshi");
    }

    #[test]
    #[should_panic(expected = "required")]
    fn credentials_only_password_panics() {
        parse_server_config("para server --password secret");
    }

    #[test]
    fn credentials_mutual_requirement_no_panic() {
        parse_server_config("para server --username satoshi --password secret");
        parse_server_config("para server");
    }

    #[test]
    fn default_nodes() {
        let config = parse_server_config("para server");
        assert!(config.nodes().is_empty());
    }

    #[test]
    fn override_nodes_single_http() {
        let config = parse_server_config("para server --nodes http://localhost:80");
        let expected = vec![Url::parse("http://localhost:80").unwrap()];
        assert_eq!(config.nodes(), expected);
    }

    #[test]
    fn override_nodes_single_https() {
        let config = parse_server_config("para server --nodes https://parasite.wtf");
        let expected = vec![Url::parse("https://parasite.wtf").unwrap()];
        assert_eq!(config.nodes(), expected);
    }

    #[test]
    fn multiple_nodes() {
        let config = parse_server_config(
            "para server --nodes http://localhost:80 --nodes https://parasite.wtf",
        );
        let expected = vec![
            Url::parse("http://localhost:80").unwrap(),
            Url::parse("https://parasite.wtf").unwrap(),
        ];
        assert_eq!(config.nodes(), expected);
    }

    #[test]
    #[should_panic(expected = "error parsing arguments")]
    fn invalid_node_url() {
        parse_server_config("para server --nodes invalid_url");
    }

    #[test]
    fn test_zero_seconds() {
        assert_eq!(format_uptime(0), "0 minutes");
    }

    #[test]
    fn test_single_units() {
        assert_eq!(format_uptime(1), "0 minutes");
        assert_eq!(format_uptime(60), "1 minute");
        assert_eq!(format_uptime(3600), "1 hour");
        assert_eq!(format_uptime(86400), "1 day");
    }

    #[test]
    fn test_plural_units() {
        assert_eq!(format_uptime(120), "2 minutes");
        assert_eq!(format_uptime(7200), "2 hours");
        assert_eq!(format_uptime(172800), "2 days");
    }

    #[test]
    fn test_mixed_units() {
        assert_eq!(format_uptime(90060), "1 day, 1 hour, 1 minute");
        assert_eq!(format_uptime(183900), "2 days, 3 hours, 5 minutes");
        assert_eq!(format_uptime(88200), "1 day, 30 minutes");
        assert_eq!(format_uptime(8100), "2 hours, 15 minutes");
    }

    #[test]
    fn test_edge_cases() {
        assert_eq!(format_uptime(59), "0 minutes");
        assert_eq!(format_uptime(3599), "59 minutes");
        assert_eq!(format_uptime(86399), "23 hours, 59 minutes");
        assert_eq!(format_uptime(60), "1 minute");
        assert_eq!(format_uptime(3600), "1 hour");
        assert_eq!(format_uptime(86400), "1 day");
    }

    #[test]
    fn test_large_values() {
        assert_eq!(format_uptime(2592000), "30 days");
        assert_eq!(format_uptime(31581000), "365 days, 12 hours, 30 minutes");
    }

    #[test]
    fn test_only_minutes_when_less_than_hour() {
        assert_eq!(format_uptime(30), "0 minutes");
        assert_eq!(format_uptime(90), "1 minute");
        assert_eq!(format_uptime(1800), "30 minutes");
    }

    #[test]
    fn test_fractional_seconds_truncated() {
        assert_eq!(format_uptime(119), "1 minute"); // 1 min 59 sec -> 1 minute
        assert_eq!(format_uptime(3659), "1 hour"); // 1 hour 59 sec -> 1 hour
        assert_eq!(format_uptime(86459), "1 day"); // 1 day 59 sec -> 1 day
    }

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
