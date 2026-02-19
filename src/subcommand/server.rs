use {
    super::*,
    crate::{
        ckpool,
        http_server::{
            self, HttpConfig,
            accept_json::AcceptJson,
            error::{OptionExt, ServerError, ServerResult},
        },
        subcommand::{
            server::{
                account::account_router, payouts::payouts_router,
                sharediff::share_difficulty_router, sync_routes::sync_router,
            },
            sync::{ShareBatch, SyncResponse},
        },
    },
    aggregator::Aggregator,
    axum::extract::{Path, Query},
    cache::Cache,
    database::Database,
    reqwest::{Client, ClientBuilder, header},
    server_config::ServerConfig,
    std::sync::OnceLock,
    sysinfo::DiskRefreshKind,
    templates::{
        PageContent, PageHtml, aggregator_dashboard::AggregatorDashboardHtml, home::HomeHtml,
        payouts::PayoutsHtml, status::StatusHtml,
    },
    tower_http::{
        services::ServeDir, set_header::SetResponseHeaderLayer,
        validate_request::ValidateRequestHeaderLayer,
    },
    utoipa::{
        Modify,
        openapi::security::{Http, HttpAuthScheme, SecurityScheme},
    },
};

pub mod account;
mod aggregator;
mod cache;
pub mod database;
pub mod notifications;
mod payouts;
mod server_config;
mod sharediff;
mod sync_routes;
mod templates;

const MEBIBYTE: usize = 1 << 20;
const BUDGET: Duration = Duration::from_secs(15);
const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_ATTEMPTS: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
static MIGRATION_DONE: OnceLock<bool> = OnceLock::new();

#[allow(deprecated)]
pub(crate) fn bearer_auth<T: Default>(
    token: &str,
) -> ValidateRequestHeaderLayer<tower_http::auth::require_authorization::Bearer<T>> {
    ValidateRequestHeaderLayer::bearer(token)
}

fn exclusion_list_from_params(params: HashMap<String, String>) -> Vec<String> {
    params
        .get("excluded")
        .map(|s| s.split(',').map(String::from).collect::<Vec<_>>())
        .unwrap_or_default()
}

pub type Status = StatusHtml;

#[derive(Debug)]
struct AccountUpdate {
    username: String,
    lnurl: Option<String>,
    total_diff: f64,
    blockheights: HashSet<i32>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub(crate) struct Payment {
    pub(crate) lightning_address: String,
    pub(crate) amount: i64,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub(crate) struct SatSplit {
    pub(crate) block_height: i32,
    pub(crate) block_hash: String,
    pub(crate) total_payment_amount: i64,
    pub(crate) payments: Vec<Payment>,
}

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "api_token",
                SecurityScheme::Http(
                    Http::builder()
                        .scheme(HttpAuthScheme::Bearer)
                        .description(Some("API token for general endpoints"))
                        .build(),
                ),
            );
            components.add_security_scheme(
                "admin_token",
                SecurityScheme::Http(
                    Http::builder()
                        .scheme(HttpAuthScheme::Bearer)
                        .description(Some("Admin token for privileged operations"))
                        .build(),
                ),
            );
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Parasite Pool API",
        version = env!("CARGO_PKG_VERSION"),
        description = "Mining pool API for share tracking, payouts, and account management"
    ),
    modifiers(&SecurityAddon),
    paths(
        // Account endpoints
        account::account_lookup,
        account::account_update,
        account::account_metadata_update,
        // Share difficulty endpoints
        sharediff::highestdiff,
        sharediff::highestdiff_by_user,
        sharediff::highestdiff_all_users,
        sharediff::get_tera_shares,
        // Payout endpoints
        payouts::payouts_all,
        payouts::payouts_failed,
        payouts::payouts,
        payouts::open_split,
        payouts::sat_split,
        payouts::payouts_range,
        payouts::user_payout_range,
        payouts::update_payout_status,
        payouts::payouts_simulate,
        // Sync endpoints
        sync_routes::sync_batch,
        // Status endpoints
        status,
        // Aggregator endpoints
        aggregator::blockheight,
        aggregator::pool_status,
        aggregator::user_status,
        aggregator::users,
    ),
    components(schemas(
        // Account schemas
        account::Account,
        account::AccountUpdate,
        account::AccountMetadataUpdate,
        account::AccountResponse,
        // Database schemas
        database::HighestDiff,
        database::TeraShare,
        database::Split,
        database::Payout,
        database::PendingPayout,
        database::FailedPayout,
        database::UpdatePayoutStatusRequest,
        // Server schemas
        Payment,
        SatSplit,
        // Sync schemas (Sent from Sync)
        ShareBatch,
        SyncResponse,
        // Status schema
        StatusHtml,
        // Aggregator schemas
        ckpool::User,
        ckpool::Worker,
    )),
    tags(
        (name = "account", description = "Account management endpoints"),
        (name = "sharediff", description = "Share difficulty endpoints"),
        (name = "payouts", description = "Payout and split endpoints"),
        (name = "sync", description = "Share synchronization endpoints"),
        (name = "status", description = "Server status endpoints"),
        (name = "aggregator", description = "Multi-node aggregation endpoints"),
    )
)]
pub struct ApiDoc;

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

#[derive(Clone, Debug, Parser)]
pub struct Server {
    #[command(flatten)]
    pub(crate) config: ServerConfig,
}

impl Server {
    pub async fn run(&self, handle: Handle, cancel_token: CancellationToken) -> Result {
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

        let shutdown_handle = handle.clone();
        tokio::spawn(async move {
            cancel_token.cancelled().await;
            info!("Received shutdown signal, stopping server...");
            shutdown_handle.shutdown();
        });

        let mut router = Router::new()
            .nest_service("/pool/", ServeDir::new(pool_dir))
            .nest_service("/users/", ServeDir::new(user_dir))
            .route("/users", get(Self::users))
            .layer(SetResponseHeaderLayer::overriding(
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain"),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                CONTENT_DISPOSITION,
                HeaderValue::from_static("inline"),
            ));

        router = if let Some(token) = config.api_token() {
            router.layer(bearer_auth(token))
        } else {
            router
        };

        router = router
            .route("/", get(Self::home))
            .route("/status", Self::with_auth(config.clone(), get(status)))
            .route("/static/{*path}", get(Self::static_assets));

        #[cfg(feature = "swagger-ui")]
        {
            router = router.merge(
                utoipa_swagger_ui::SwaggerUi::new("/swagger-ui/")
                    .url("/api-docs/openapi.json", ApiDoc::openapi()),
            );
        }

        match Database::new(config.database_url()).await {
            Ok(database) => {
                if config.migrate_accounts() {
                    let pool = database.pool.clone();
                    tokio::spawn(async move {
                        info!("Starting account migration worker...");
                        match sqlx::query_scalar::<_, i64>("SELECT refresh_accounts()")
                            .fetch_one(&pool)
                            .await
                        {
                            Ok(rows_affected) => {
                                info!(
                                    "Account migration completed. {} accounts affected.",
                                    rows_affected
                                );
                            }
                            Err(e) => {
                                error!("Account migration failed: {}", e);
                            }
                        }
                        let _ = MIGRATION_DONE.set(true);
                    });
                }

                router = router
                    .merge(account_router(config.clone(), database.clone()))
                    .merge(share_difficulty_router(config.clone(), database.clone()))
                    .merge(payouts_router(config.clone(), database.clone()))
                    .merge(sync_router(config.clone(), database));
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

        let acme_domains = config.domains()?;
        let acme_contacts = config.acme_contacts();
        let tls_enabled = !acme_domains.is_empty() && !acme_contacts.is_empty();

        let http_config = HttpConfig {
            address: config.address(),
            port: config.port().unwrap_or(if tls_enabled { 443 } else { 80 }),
            acme_domains,
            acme_contacts,
            acme_cache: config.acme_cache(),
        };

        http_server::spawn_with_handle(http_config, router, handle)?.await??;

        Ok(())
    }

    fn with_auth<S>(config: Arc<ServerConfig>, method_router: MethodRouter<S>) -> MethodRouter<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        if let Some(token) = config.admin_token() {
            method_router.layer(bearer_auth(token))
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

    pub(crate) async fn static_assets(path: Path<String>) -> ServerResult<Response> {
        http_server::static_assets(path).await
    }

    async fn get_synced_blockheight(config: &ServerConfig) -> Option<i32> {
        let id_file = config.data_dir().join("current_id.txt");
        let id_file_str = id_file.to_string_lossy();

        let current_id = match sync::load_current_id_from_file(&id_file_str).await {
            Ok(id) => id,
            Err(e) => {
                warn!("Failed to load current sync id: {}", e);
                return None;
            }
        };

        if current_id == 0 {
            return None;
        }

        let database = match Database::new(config.database_url()).await {
            Ok(db) => db,
            Err(e) => {
                warn!(
                    "Failed to connect to database for blockheight lookup: {}",
                    e
                );
                return None;
            }
        };

        match database.get_blockheight_for_id(current_id).await {
            Ok(blockheight) => blockheight,
            Err(e) => {
                warn!("Failed to get blockheight for id {}: {}", current_id, e);
                None
            }
        }
    }
}

/// Get server status
#[utoipa::path(
    get,
    path = "/status",
    security(("admin_token" = [])),
    responses(
        (status = 200, description = "Server status", body = StatusHtml),
    ),
    tag = "status"
)]
pub(crate) async fn status(
    Extension(config): Extension<Arc<ServerConfig>>,
    AcceptJson(accept_json): AcceptJson,
) -> ServerResult<Response> {
    let blockheight = Server::get_synced_blockheight(&config).await;

    task::block_in_place(|| {
        let mut system = System::new_all();
        system.refresh_all();

        let path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

        let mut disk_usage_percent = 0.0;
        let disks =
            Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage());
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

        let status_file = config.log_dir().join("pool/pool.status");

        let parsed_status = fs::read_to_string(&status_file)
            .ok()
            .and_then(|s| ckpool::Status::from_str(&s).ok());

        let status = StatusHtml {
            disk_usage_percent,
            memory_usage_percent,
            cpu_usage_percent,
            uptime: System::uptime(),
            hashrate: parsed_status.map(|st| st.hash_rates.hashrate1m),
            users: parsed_status.map(|st| st.pool.users),
            workers: parsed_status.map(|st| st.pool.workers),
            accepted: parsed_status.map(|st| st.shares.accepted),
            rejected: parsed_status.map(|st| st.shares.rejected),
            best_share: parsed_status.map(|st| st.shares.bestshare),
            sps: parsed_status.map(|st| st.shares.sps1m),
            total_work: parsed_status.map(|st| st.shares.diff),
            blockheight,
        };

        Ok(if accept_json {
            Json(status).into_response()
        } else {
            status.page(config.domain()).into_response()
        })
    })
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
    fn default_no_admin_token() {
        let config = parse_server_config("para server");
        assert_eq!(config.admin_token(), None);
    }

    #[test]
    fn admin_token() {
        let config = parse_server_config("para server --admin-token verysecrettoken");
        assert_eq!(config.admin_token(), Some("verysecrettoken"));
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
