use {
    super::*,
    crate::{HashRate, metatron::Metatron},
    axum::{
        Json, Router,
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
        routing::get,
    },
    serde::{Deserialize, Serialize},
    std::sync::Arc,
};

/// Aggregate pool statistics.
///
/// Returned by the `GET /api/stats` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStats {
    pub hash_rate: HashRate,
    pub shares_per_second: f64,
    pub users: usize,
    pub workers: usize,
    pub connections: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub blocks: u64,
    pub best_ever: f64,
    pub uptime_secs: u64,
}

/// Summary information for a user.
///
/// Returned by the `GET /api/users` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSummary {
    pub address: String,
    pub hash_rate: HashRate,
    pub shares_per_second: f64,
    pub workers: usize,
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: f64,
}

/// Detailed information for a user, including their workers.
///
/// Returned by the `GET /api/users/:address` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDetail {
    pub address: String,
    pub hash_rate: HashRate,
    pub shares_per_second: f64,
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: f64,
    pub workers: Vec<WorkerSummary>,
}

/// Summary information for a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSummary {
    pub name: String,
    pub hash_rate: HashRate,
    pub shares_per_second: f64,
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: f64,
}

/// Creates the API router with all endpoints.
pub fn create_router(metatron: Arc<Metatron>) -> Router {
    Router::new()
        .route("/api/stats", get(api_stats))
        .route("/api/users", get(api_users))
        .route("/api/users/{address}", get(api_user))
        .with_state(metatron)
}

async fn api_stats(State(metatron): State<Arc<Metatron>>) -> Json<PoolStats> {
    Json(metatron.stats())
}

async fn api_users(State(metatron): State<Arc<Metatron>>) -> Json<Vec<UserSummary>> {
    Json(metatron.users())
}

async fn api_user(
    State(metatron): State<Arc<Metatron>>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    match metatron.user(&address) {
        Some(user) => Ok(Json(user)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// Configuration for the HTTP API server.
#[derive(Clone, Debug)]
pub struct HttpConfig {
    pub address: String,
    pub port: u16,
    pub acme_domains: Vec<String>,
    pub acme_contacts: Vec<String>,
    pub acme_cache: PathBuf,
}

impl HttpConfig {
    /// Returns true if TLS should be enabled (both domains and contacts are configured).
    #[allow(unused)]
    pub fn tls_enabled(&self) -> bool {
        !self.acme_domains.is_empty() && !self.acme_contacts.is_empty()
    }
}

/// Spawns the HTTP API server.
///
/// If `acme_domains` and `acme_contacts` are both non-empty, the server will use
/// HTTPS with automatic Let's Encrypt certificates. Otherwise, plain HTTP.
pub fn spawn(
    config: HttpConfig,
    metatron: Arc<Metatron>,
    cancel_token: CancellationToken,
) -> Result<task::JoinHandle<io::Result<()>>> {
    let router = api::create_router(metatron);
    let handle = Handle::new();

    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        cancel_token.cancelled().await;
        info!("Received shutdown signal, stopping HTTP API server...");
        shutdown_handle.shutdown();
    });

    spawn_server(config, router, handle)
}

fn spawn_server(
    config: HttpConfig,
    router: Router,
    handle: Handle,
) -> Result<JoinHandle<io::Result<()>>> {
    let address = config.address.clone();
    let port = config.port;
    let acme_domains = config.acme_domains.clone();
    let acme_contacts = config.acme_contacts.clone();
    let acme_cache = config.acme_cache.clone();

    Ok(tokio::spawn(async move {
        if !acme_domains.is_empty() && !acme_contacts.is_empty() {
            info!(
                "Getting certificate for {} using contact email {}",
                acme_domains[0], acme_contacts[0]
            );

            let addr = (address.as_str(), port).to_socket_addrs()?.next().unwrap();

            info!("HTTP API listening on https://{addr}");

            axum_server::Server::bind(addr)
                .handle(handle)
                .acceptor(acceptor(acme_domains, acme_contacts, acme_cache).unwrap())
                .serve(router.into_make_service())
                .await
        } else {
            let addr = (address.as_str(), port).to_socket_addrs()?.next().unwrap();

            info!("HTTP API listening on http://{addr}");

            axum_server::Server::bind(addr)
                .handle(handle)
                .serve(router.into_make_service())
                .await
        }
    }))
}

fn acceptor(
    acme_domains: Vec<String>,
    acme_contacts: Vec<String>,
    acme_cache: PathBuf,
) -> Result<AxumAcceptor> {
    static RUSTLS_PROVIDER_INSTALLED: LazyLock<bool> = LazyLock::new(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .is_ok()
    });

    let config = AcmeConfig::new(acme_domains)
        .contact(acme_contacts)
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
        loop {
            match state.next().await {
                Some(Ok(ok)) => info!("ACME event: {:?}", ok),
                Some(Err(err)) => error!("ACME error: {:?}", err),
                None => break,
            }
        }
    });

    Ok(acceptor)
}
