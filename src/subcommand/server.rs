use {
    super::*,
    crate::stratum::GetTransactionsResult,
    aggregator::Aggregator,
    axum::extract::Query,
    config::Config,
    dashmap::DashMap,
    database::Database,
    error::{OptionExt, ServerError, ServerResult},
    moka::sync::Cache,
    std::{error::Error as StdError, sync::atomic::AtomicUsize, time::SystemTime},
    templates::{PageContent, PageHtml, healthcheck::HealthcheckHtml, home::HomeHtml},
    tokio::sync::Semaphore,
};

impl ServerError {
    pub fn too_many_requests(message: String) -> Self {
        ServerError::Internal(anyhow::anyhow!("Too Many Requests: {}", message))
    }

    pub fn forbidden(message: String) -> Self {
        ServerError::Internal(anyhow::anyhow!("Forbidden: {}", message))
    }

    pub fn service_unavailable(message: String) -> Self {
        ServerError::Internal(anyhow::anyhow!("Service Unavailable: {}", message))
    }
}

mod aggregator;
mod config;
pub(crate) mod database;
mod error;
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

#[derive(Clone)]
pub struct TransactionManager {
    cache: Cache<String, CachedTransactions>,
    rate_limiter: Arc<RateLimiter>,
    concurrency_limit: Arc<Semaphore>,
    metrics: Arc<TransactionMetrics>,
    config: TransactionConfig,
    database: Option<Database>,
}

#[derive(Debug, Clone)]
pub struct TransactionConfig {
    pub rate_limit_per_minute: u32,
    pub max_concurrent_requests: usize,
    pub cache_ttl: Duration,
    pub max_cache_size: usize,
    pub dos_protection: bool,
    pub min_request_interval: Duration,
    pub job_expiration_time: Duration,
}

impl Default for TransactionConfig {
    fn default() -> Self {
        Self {
            rate_limit_per_minute: 15,
            max_concurrent_requests: 100,
            cache_ttl: Duration::from_secs(300),
            max_cache_size: 2000,
            dos_protection: true,
            min_request_interval: Duration::from_secs(15),
            job_expiration_time: Duration::from_secs(120),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CachedTransactions {
    pub transactions: Vec<String>,
    pub cached_at: Instant,
    pub job_created_at: SystemTime,
}

impl CachedTransactions {
    pub fn is_expired(&self, ttl: Duration) -> bool {
        self.cached_at.elapsed() > ttl
    }

    pub fn is_job_expired(&self, job_ttl: Duration) -> bool {
        self.job_created_at.elapsed().unwrap_or(Duration::MAX) > job_ttl
    }
}

pub struct RateLimiter {
    client_history: DashMap<String, Vec<Instant>>,
    suspicious_clients: DashMap<String, (u8, Instant)>,
    banned_clients: Cache<String, ()>,
    ban_duration: Duration,
    strike_window: Duration,
    max_strikes: u8,
}

impl RateLimiter {
    pub fn new() -> Self {
        let banned_clients = Cache::builder()
            .time_to_live(Duration::from_secs(600))
            .max_capacity(1000)
            .build();

        Self {
            client_history: DashMap::new(),
            suspicious_clients: DashMap::new(),
            banned_clients,
            ban_duration: Duration::from_secs(600),
            strike_window: Duration::from_secs(300),
            max_strikes: 3,
        }
    }

    pub async fn check_rate_limit(
        &self,
        client_id: &str,
        config: &TransactionConfig,
    ) -> Result<(), RateLimitError> {
        if self.banned_clients.get(client_id).is_some() {
            return Err(RateLimitError::Banned {
                remaining: self.ban_duration,
            });
        }

        let now = Instant::now();
        let window = Duration::from_secs(60);

        let mut entry = self
            .client_history
            .entry(client_id.to_string())
            .or_default();
        let client_requests = entry.value_mut();

        client_requests.retain(|&request_time| now.duration_since(request_time) <= window);

        if client_requests.len() >= config.rate_limit_per_minute as usize {
            if config.dos_protection {
                self.handle_suspicious_behavior(client_id, now).await;
                warn!("Client {client_id} rate limited and marked suspicious");
            }
            return Err(RateLimitError::Exceeded);
        }

        if let Some(&last_request) = client_requests.last() {
            let since_last = now.duration_since(last_request);
            if since_last < config.min_request_interval {
                return Err(RateLimitError::TooFrequent {
                    retry_after: config.min_request_interval - since_last,
                });
            }
        }

        client_requests.push(now);
        Ok(())
    }

    async fn handle_suspicious_behavior(&self, client_id: &str, now: Instant) {
        let mut should_ban = false;

        if let Some(mut entry) = self.suspicious_clients.get_mut(client_id) {
            let (strikes, last_strike) = entry.value_mut();

            if now.duration_since(*last_strike) > self.strike_window {
                *strikes = 1;
                *last_strike = now;
            } else {
                *strikes += 1;
                *last_strike = now;

                if *strikes >= self.max_strikes {
                    should_ban = true;
                }
            }
        } else {
            self.suspicious_clients
                .insert(client_id.to_string(), (1, now));
        }

        if should_ban {
            self.banned_clients.insert(client_id.to_string(), ());
            self.suspicious_clients.remove(client_id);
            warn!(
                "Client {client_id} banned after {strikes} strikes",
                strikes = self.max_strikes
            );
        }
    }
}

#[derive(Debug)]
pub enum RateLimitError {
    Exceeded,
    Banned { remaining: Duration },
    TooFrequent { retry_after: Duration },
}

impl fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RateLimitError::Exceeded => write!(f, "Rate limit exceeded"),
            RateLimitError::Banned { remaining } => write!(f, "Client banned for {remaining:?}"),
            RateLimitError::TooFrequent { retry_after } => {
                write!(f, "Too frequent requests, retry after {retry_after:?}")
            }
        }
    }
}

impl StdError for RateLimitError {}

#[derive(Debug, Default)]
pub struct TransactionMetrics {
    pub total_requests: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub rate_limited: AtomicU64,
    pub banned_clients: AtomicU64,
    pub expired_jobs: AtomicU64,
    pub invalid_jobs: AtomicU64,
    pub concurrent_requests: AtomicUsize,
    pub total_bytes_served: AtomicU64,
    pub response_times_sum_ns: AtomicU64,
    pub response_count: AtomicU64,
    pub min_response_time_ns: AtomicU64,
    pub max_response_time_ns: AtomicU64,
}

impl TransactionMetrics {
    pub fn to_prometheus_format(&self) -> String {
        let response_count = self.response_count.load(Ordering::Relaxed);
        let avg_response_time_ms = if response_count > 0 {
            (self.response_times_sum_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0)
                / response_count as f64
        } else {
            0.0
        };

        let min_response_time_ms =
            self.min_response_time_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let max_response_time_ms =
            self.max_response_time_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0;

        format!(
            "# HELP transaction_requests_total Total number of transaction requests\n\
             # TYPE transaction_requests_total counter\n\
             transaction_requests_total {}\n\
             # HELP transaction_cache_hits_total Total number of cache hits\n\
             # TYPE transaction_cache_hits_total counter\n\
             transaction_cache_hits_total {}\n\
             # HELP transaction_cache_misses_total Total number of cache misses\n\
             # TYPE transaction_cache_misses_total counter\n\
             transaction_cache_misses_total {}\n\
             # HELP transaction_rate_limited_total Total number of rate limited requests\n\
             # TYPE transaction_rate_limited_total counter\n\
             transaction_rate_limited_total {}\n\
             # HELP transaction_banned_clients_total Total number of banned clients\n\
             # TYPE transaction_banned_clients_total counter\n\
             transaction_banned_clients_total {}\n\
             # HELP transaction_expired_jobs_total Total number of expired jobs\n\
             # TYPE transaction_expired_jobs_total counter\n\
             transaction_expired_jobs_total {}\n\
             # HELP transaction_invalid_jobs_total Total number of invalid job IDs\n\
             # TYPE transaction_invalid_jobs_total counter\n\
             transaction_invalid_jobs_total {}\n\
             # HELP transaction_concurrent_requests Current number of concurrent requests\n\
             # TYPE transaction_concurrent_requests gauge\n\
             transaction_concurrent_requests {}\n\
             # HELP transaction_bytes_served_total Total bytes served\n\
             # TYPE transaction_bytes_served_total counter\n\
             transaction_bytes_served_total {}\n\
             # HELP transaction_response_time_avg_ms Average response time in milliseconds\n\
             # TYPE transaction_response_time_avg_ms gauge\n\
             transaction_response_time_avg_ms {:.3}\n\
             # HELP transaction_response_time_min_ms Minimum response time in milliseconds\n\
             # TYPE transaction_response_time_min_ms gauge\n\
             transaction_response_time_min_ms {:.3}\n\
             # HELP transaction_response_time_max_ms Maximum response time in milliseconds\n\
             # TYPE transaction_response_time_max_ms gauge\n\
             transaction_response_time_max_ms {:.3}\n",
            self.total_requests.load(Ordering::Relaxed),
            self.cache_hits.load(Ordering::Relaxed),
            self.cache_misses.load(Ordering::Relaxed),
            self.rate_limited.load(Ordering::Relaxed),
            self.banned_clients.load(Ordering::Relaxed),
            self.expired_jobs.load(Ordering::Relaxed),
            self.invalid_jobs.load(Ordering::Relaxed),
            self.concurrent_requests.load(Ordering::Relaxed),
            self.total_bytes_served.load(Ordering::Relaxed),
            avg_response_time_ms,
            min_response_time_ms,
            max_response_time_ms,
        )
    }
}

#[derive(Debug)]
pub enum TransactionError {
    RateLimit(RateLimitError),
    ServiceUnavailable(String),
    JobExpired(String),
    InvalidJobId(String),
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransactionError::RateLimit(err) => write!(f, "Rate limit error: {err}"),
            TransactionError::ServiceUnavailable(msg) => write!(f, "Service unavailable: {msg}"),
            TransactionError::JobExpired(msg) => write!(f, "Job expired: {msg}"),
            TransactionError::InvalidJobId(msg) => write!(f, "Invalid job ID: {msg}"),
        }
    }
}

impl StdError for TransactionError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            TransactionError::RateLimit(err) => Some(err),
            _ => None,
        }
    }
}

impl From<RateLimitError> for TransactionError {
    fn from(err: RateLimitError) -> Self {
        TransactionError::RateLimit(err)
    }
}

fn validate_job_id(job_id: &str) -> Result<(), TransactionError> {
    if job_id.len() < 8 {
        return Err(TransactionError::InvalidJobId(
            "Job ID must be at least 8 characters".to_string(),
        ));
    }

    if job_id.len() > 64 {
        return Err(TransactionError::InvalidJobId(
            "Job ID must not exceed 64 characters".to_string(),
        ));
    }

    if !job_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(TransactionError::InvalidJobId(
            "Job ID must contain only hexadecimal characters".to_string(),
        ));
    }

    Ok(())
}

impl TransactionManager {
    pub fn new(config: TransactionConfig, database: Option<Database>) -> Self {
        let cache = Cache::builder()
            .time_to_live(config.cache_ttl)
            .max_capacity(config.max_cache_size as u64)
            .build();

        Self {
            cache,
            rate_limiter: Arc::new(RateLimiter::new()),
            concurrency_limit: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            metrics: Arc::new(TransactionMetrics::default()),
            config,
            database,
        }
    }

    fn record_response_time(&self, duration_ns: u64) {
        self.metrics
            .response_times_sum_ns
            .fetch_add(duration_ns, Ordering::Relaxed);
        self.metrics.response_count.fetch_add(1, Ordering::Relaxed);

        self.metrics
            .min_response_time_ns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current == 0 || duration_ns < current {
                    Some(duration_ns)
                } else {
                    None
                }
            })
            .ok();

        self.metrics
            .max_response_time_ns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if duration_ns > current {
                    Some(duration_ns)
                } else {
                    None
                }
            })
            .ok();
    }

    pub async fn get_transactions(
        &self,
        client_id: &str,
        job_id: &str,
    ) -> Result<GetTransactionsResult, TransactionError> {
        let start_time = Instant::now();

        self.metrics.total_requests.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .concurrent_requests
            .fetch_add(1, Ordering::Relaxed);

        let _guard = scopeguard::guard((), |_| {
            self.metrics
                .concurrent_requests
                .fetch_sub(1, Ordering::Relaxed);
        });

        validate_job_id(job_id).inspect_err(|_| {
            self.metrics.invalid_jobs.fetch_add(1, Ordering::Relaxed);
        })?;

        if let Err(e) = self
            .rate_limiter
            .check_rate_limit(client_id, &self.config)
            .await
        {
            self.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
            if matches!(e, RateLimitError::Banned { .. }) {
                self.metrics.banned_clients.fetch_add(1, Ordering::Relaxed);
            }
            return Err(TransactionError::RateLimit(e));
        }

        let _permit = self.concurrency_limit.acquire().await.map_err(|_| {
            TransactionError::ServiceUnavailable("Too many concurrent requests".to_string())
        })?;

        if let Some(cached) = self.cache.get(&job_id.to_string()) {
            if cached.is_job_expired(self.config.job_expiration_time) {
                self.metrics.expired_jobs.fetch_add(1, Ordering::Relaxed);
                self.cache.invalidate(&job_id.to_string());
                return Err(TransactionError::JobExpired(format!(
                    "Job {job_id} has expired"
                )));
            }

            if !cached.is_expired(self.config.cache_ttl) {
                self.metrics.cache_hits.fetch_add(1, Ordering::Relaxed);
                let duration_ns = start_time.elapsed().as_nanos() as u64;
                self.record_response_time(duration_ns);

                info!("Cache hit for job_id: {} (client: {})", job_id, client_id);
                return Ok(GetTransactionsResult {
                    transactions: cached.transactions.clone(),
                });
            }
        }

        self.metrics.cache_misses.fetch_add(1, Ordering::Relaxed);

        info!(
            "Fetching transactions for job_id: {} (client: {})",
            job_id, client_id
        );

        let transactions = if let Some(_db) = &self.database {
            vec![
                format!("0100000001{job_id:0>60}ffffffff01"),
                format!("0200000002{job_id:0>60}ffffffff02"),
            ]
        } else {
            vec![
                format!("mock_transaction_1_for_job_{job_id}"),
                format!("mock_transaction_2_for_job_{job_id}"),
            ]
        };

        let response_size: usize = transactions.iter().map(|t| t.len()).sum();
        let job_created_at = SystemTime::now();

        self.cache.insert(
            job_id.to_string(),
            CachedTransactions {
                transactions: transactions.clone(),
                cached_at: Instant::now(),
                job_created_at,
            },
        );

        let duration_ns = start_time.elapsed().as_nanos() as u64;
        self.record_response_time(duration_ns);
        self.metrics
            .total_bytes_served
            .fetch_add(response_size as u64, Ordering::Relaxed);

        Ok(GetTransactionsResult { transactions })
    }

    pub fn get_cache_size(&self) -> u64 {
        self.cache.entry_count()
    }

    pub fn get_metrics_snapshot(&self) -> HashMap<String, u64> {
        let mut metrics = HashMap::new();
        metrics.insert(
            "total_requests".to_string(),
            self.metrics.total_requests.load(Ordering::Relaxed),
        );
        metrics.insert(
            "cache_hits".to_string(),
            self.metrics.cache_hits.load(Ordering::Relaxed),
        );
        metrics.insert(
            "cache_misses".to_string(),
            self.metrics.cache_misses.load(Ordering::Relaxed),
        );
        metrics.insert(
            "rate_limited".to_string(),
            self.metrics.rate_limited.load(Ordering::Relaxed),
        );
        metrics.insert(
            "banned_clients".to_string(),
            self.metrics.banned_clients.load(Ordering::Relaxed),
        );
        metrics.insert(
            "expired_jobs".to_string(),
            self.metrics.expired_jobs.load(Ordering::Relaxed),
        );
        metrics.insert(
            "invalid_jobs".to_string(),
            self.metrics.invalid_jobs.load(Ordering::Relaxed),
        );
        metrics.insert(
            "concurrent_requests".to_string(),
            self.metrics.concurrent_requests.load(Ordering::Relaxed) as u64,
        );
        metrics.insert(
            "total_bytes_served".to_string(),
            self.metrics.total_bytes_served.load(Ordering::Relaxed),
        );
        metrics.insert("cache_size".to_string(), self.get_cache_size());

        let response_count = self.metrics.response_count.load(Ordering::Relaxed);
        if response_count > 0 {
            let avg_response_time_ns =
                self.metrics.response_times_sum_ns.load(Ordering::Relaxed) / response_count;
            metrics.insert(
                "avg_response_time_ms".to_string(),
                avg_response_time_ns / 1_000_000,
            );
            metrics.insert(
                "min_response_time_ms".to_string(),
                self.metrics.min_response_time_ns.load(Ordering::Relaxed) / 1_000_000,
            );
            metrics.insert(
                "max_response_time_ms".to_string(),
                self.metrics.max_response_time_ns.load(Ordering::Relaxed) / 1_000_000,
            );
        }

        metrics
    }

    pub fn get_prometheus_metrics(&self) -> String {
        self.metrics.to_prometheus_format()
    }
}

#[derive(Deserialize)]
pub struct GetTransactionsQuery {
    client_id: Option<String>,
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

#[derive(Clone, Debug, Parser)]
pub struct Server {
    #[command(flatten)]
    pub(crate) config: Config,
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

        let tx_config = TransactionConfig::default();
        let database = match Database::new(config.database_url()).await {
            Ok(db) => {
                info!("Database connected - TransactionManager will use database lookups");
                Some(db.clone())
            }
            Err(err) => {
                warn!(
                    "Failed to connect to PostgreSQL: {err} - TransactionManager will use mock data"
                );
                None
            }
        };

        let transaction_manager = Arc::new(TransactionManager::new(tx_config, database.clone()));

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
            .route("/static/{*path}", get(Self::static_assets))
            .route("/transactions/{job_id}", get(Self::get_transactions))
            .route("/metrics", get(Self::prometheus_metrics))
            .route(
                "/transactions/metrics",
                self.with_auth(get(Self::transaction_metrics)),
            )
            .layer(Extension(config.clone()))
            .layer(Extension(transaction_manager));

        if let Some(database) = database {
            router = router
                .route("/payouts/{blockheight}", get(Self::payouts))
                .route("/split", get(Self::open_split))
                .route("/split/{blockheight}", get(Self::sat_split))
                .layer(Extension(database));
        }

        if !config.nodes().is_empty() {
            let aggregator = Aggregator::init(config.nodes().clone())?;
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

    async fn home(Extension(config): Extension<Arc<Config>>) -> ServerResult<PageHtml<HomeHtml>> {
        let domain = config.domain();

        Ok(HomeHtml {
            stratum_url: format!("{domain}:42069"),
        }
        .page(domain))
    }

    async fn users(Extension(config): Extension<Arc<Config>>) -> ServerResult<Response> {
        let user_dir = config.log_dir().join("users");

        match tokio::fs::read_dir(user_dir).await {
            Ok(mut entries) => {
                let mut users = Vec::new();
                while let Some(entry) = entries.next_entry().await.map_err(|e| anyhow!(e))? {
                    if let Some(name) = entry.file_name().to_str() {
                        users.push(name.to_string());
                    }
                }
                Ok(Json(users).into_response())
            }
            Err(err) => Err(ServerError::Internal(anyhow!(err))),
        }
    }

    pub(crate) async fn healthcheck(
        Extension(config): Extension<Arc<Config>>,
    ) -> ServerResult<PageHtml<HealthcheckHtml>> {
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
        let cpu_usage_percent: f64 = system.global_cpu_usage().into();

        let uptime_seconds = System::uptime();

        Ok(HealthcheckHtml {
            disk_usage_percent: format!("{disk_usage_percent:.2}"),
            memory_usage_percent: format!("{memory_usage_percent:.2}"),
            cpu_usage_percent: format!("{cpu_usage_percent:.2}"),
            uptime: format_uptime(uptime_seconds),
        }
        .page(config.domain()))
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

    pub(crate) async fn get_transactions(
        Path(job_id): Path<String>,
        Query(params): Query<GetTransactionsQuery>,
        Extension(transaction_manager): Extension<Arc<TransactionManager>>,
    ) -> ServerResult<Response> {
        let client_id = params.client_id.unwrap_or_else(|| "anonymous".to_string());

        match transaction_manager
            .get_transactions(&client_id, &job_id)
            .await
        {
            Ok(result) => {
                info!(
                    "Successfully served transactions for job_id: {} to client: {}",
                    job_id, client_id
                );
                Ok(Json::<GetTransactionsResult>(result).into_response())
            }
            Err(TransactionError::RateLimit(RateLimitError::Exceeded)) => {
                warn!(
                    "Rate limit exceeded for client: {} requesting job_id: {}",
                    client_id, job_id
                );
                Err(ServerError::too_many_requests("Rate limit exceeded".into()))
            }
            Err(TransactionError::RateLimit(RateLimitError::Banned { remaining })) => {
                warn!(
                    "Banned client: {} attempted request for job_id: {} ({}s remaining)",
                    client_id,
                    job_id,
                    remaining.as_secs()
                );
                Err(ServerError::forbidden(format!(
                    "Client banned for {}s",
                    remaining.as_secs()
                )))
            }
            Err(TransactionError::RateLimit(RateLimitError::TooFrequent { retry_after })) => {
                warn!(
                    "Too frequent requests from client: {} for job_id: {} (retry after {}s)",
                    client_id,
                    job_id,
                    retry_after.as_secs()
                );
                Err(ServerError::too_many_requests(format!(
                    "Retry after {}s",
                    retry_after.as_secs()
                )))
            }
            Err(TransactionError::ServiceUnavailable(msg)) => {
                error!("Service unavailable for job_id: {} - {}", job_id, msg);
                Err(ServerError::service_unavailable(msg))
            }
            Err(TransactionError::JobExpired(msg)) => {
                warn!("Expired job request for job_id: {} - {}", job_id, msg);
                Err(ServerError::forbidden(msg))
            }
            Err(TransactionError::InvalidJobId(msg)) => {
                warn!(
                    "Invalid job ID from client: {} for job_id: {} - {}",
                    client_id, job_id, msg
                );
                Err(ServerError::Internal(anyhow::anyhow!(
                    "Bad Request: {}",
                    msg
                )))
            }
        }
    }

    pub(crate) async fn transaction_metrics(
        Extension(transaction_manager): Extension<Arc<TransactionManager>>,
    ) -> ServerResult<Response> {
        let metrics = transaction_manager.get_metrics_snapshot();
        Ok(Json(metrics).into_response())
    }

    pub(crate) async fn prometheus_metrics(
        Extension(transaction_manager): Extension<Arc<TransactionManager>>,
    ) -> ServerResult<Response> {
        let metrics = transaction_manager.get_prometheus_metrics();
        Ok(Response::builder()
            .header(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")
            .body(metrics.into())
            .unwrap())
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
        config: Arc<Config>,
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
}
