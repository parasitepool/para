use {
    super::*,
    crate::http_server::auth::{ApiAuth, NavbarAuth},
    axum::extract::RawQuery,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSummary {
    pub address: Address<NetworkUnchecked>,
    pub worker_count: usize,
    pub session_count: usize,
    pub hashrate: HashRate,
    pub delivered_hash_days: HashDays,
    pub best_share: Option<Difficulty>,
    pub last_share: Option<u64>,
}

impl UserSummary {
    pub(crate) fn from_user(user: &User, now: Instant) -> Self {
        let stats = user.snapshot();
        Self {
            address: user.address.as_unchecked().clone(),
            worker_count: user.worker_count(),
            session_count: user.session_count(),
            hashrate: stats.hashrate_1m(now),
            delivered_hash_days: stats.delivered_work().to_hash_days(),
            best_share: stats.best_share,
            last_share: stats.last_share_epoch_secs(now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDetail {
    pub address: Address<NetworkUnchecked>,
    pub session_count: usize,
    pub authorized_at: u64,
    pub workers: Vec<WorkerDetail>,
    pub sessions: Vec<SessionDetail>,
    pub stats: MiningStats,
}

impl UserDetail {
    pub(crate) fn from_user(user: &User, now: Instant) -> Self {
        let mut workers = Vec::new();
        let mut sessions = Vec::new();

        for worker in user.workers() {
            sessions.extend(
                worker
                    .sessions()
                    .map(|s| SessionDetail::from_session(&s, now)),
            );
            workers.push(WorkerDetail::from_worker(&worker, now));
        }

        let user_stats = user.snapshot();

        Self {
            address: user.address.as_unchecked().clone(),
            session_count: user.session_count(),
            authorized_at: user.authorized,
            workers,
            sessions,
            stats: MiningStats::from_stats(&user_stats, now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerDetail {
    pub name: String,
    pub session_count: usize,
    pub stats: MiningStats,
}

impl WorkerDetail {
    pub(crate) fn from_worker(worker: &Worker, now: Instant) -> Self {
        let stats = worker.snapshot();
        Self {
            name: worker.workername().to_string(),
            session_count: worker.session_count(),
            stats: MiningStats::from_stats(&stats, now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDetail {
    pub id: SessionId,
    pub order_id: u32,
    pub address: Address<NetworkUnchecked>,
    pub worker_name: String,
    pub username: String,
    pub enonce1: Extranonce,
    pub version_mask: Option<Version>,
    pub stats: MiningStats,
}

impl SessionDetail {
    pub(crate) fn from_session(session: &Session, now: Instant) -> Self {
        let stats = session.snapshot();
        Self {
            id: session.id(),
            order_id: session.id().order_id(),
            address: session.address().as_unchecked().clone(),
            worker_name: session.workername().to_string(),
            username: session.username().to_string(),
            enonce1: session.enonce1().clone(),
            version_mask: session.version_mask(),
            stats: MiningStats::from_stats(&stats, now),
        }
    }
}

#[derive(Copy, Clone)]
pub(crate) enum Service {
    Pool,
    Proxy,
    Router,
}

impl Service {
    fn api_base(self) -> &'static str {
        match self {
            Self::Pool => "/api/pool",
            Self::Proxy => "/api/proxy",
            Self::Router => "/api/router",
        }
    }

    fn users_title(self) -> &'static str {
        match self {
            Self::Pool => "Pool | Users",
            Self::Proxy => "Proxy | Users",
            Self::Router => "Router | Users",
        }
    }

    fn user_title(self) -> &'static str {
        match self {
            Self::Pool => "Pool | User",
            Self::Proxy => "Proxy | User",
            Self::Router => "Router | User",
        }
    }
}

#[derive(Clone)]
struct UsersState {
    service: Service,
    metatron: Arc<Metatron>,
}

pub(crate) fn routes(service: Service, metatron: Arc<Metatron>) -> axum::Router {
    axum::Router::new()
        .route("/users", get(users_page))
        .route("/user/{address}", get(user_page))
        .route(&format!("{}/users", service.api_base()), get(users))
        .route(
            &format!("{}/user/{{address}}", service.api_base()),
            get(user),
        )
        .with_state(UsersState { service, metatron })
}

async fn users_page(
    State(state): State<UsersState>,
    Extension(chain): Extension<Chain>,
    auth: NavbarAuth,
) -> Response {
    render_page(
        UsersHtml {
            title: state.service.users_title(),
            api_base: state.service.api_base(),
        },
        chain,
        auth,
    )
}

async fn user_page(
    State(state): State<UsersState>,
    Extension(chain): Extension<Chain>,
    auth: NavbarAuth,
) -> Response {
    render_page(
        UserHtml {
            title: state.service.user_title(),
            api_base: state.service.api_base(),
        },
        chain,
        auth,
    )
}

#[derive(Default)]
struct UsersQuery {
    search: Option<String>,
    limit: Option<usize>,
}

impl UsersQuery {
    fn parse(raw: Option<&str>) -> ServerResult<Self> {
        let mut query = Self::default();

        let Some(raw) = raw else {
            return Ok(query);
        };

        for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = decode_query_component(key)?;
            let value = decode_query_component(value)?;

            match key.as_str() {
                "search" if !value.trim().is_empty() => {
                    query.search = Some(value.trim().to_lowercase());
                }
                "search" => query.search = None,
                "limit" if !value.trim().is_empty() => {
                    query.limit = Some(parse_usize_query_param("limit", &value)?);
                }
                "limit" => query.limit = None,
                _ => {}
            }
        }

        Ok(query)
    }

    fn matches(&self, user: &User) -> bool {
        if let Some(search) = &self.search
            && !user_matches_search(user, search)
        {
            return false;
        }

        true
    }
}

fn user_matches_search(user: &User, search: &str) -> bool {
    user.address.to_string().to_lowercase().contains(search)
        || user
            .workers
            .iter()
            .any(|worker| worker.key().to_lowercase().contains(search))
}

async fn users(
    _: ApiAuth,
    State(state): State<UsersState>,
    RawQuery(raw_query): RawQuery,
) -> ServerResult<Response> {
    let now = Instant::now();
    let query = UsersQuery::parse(raw_query.as_deref())?;

    let mut users = Vec::new();

    for entry in state.metatron.users().iter() {
        let user = entry.value();

        if query.matches(user) {
            users.push(UserSummary::from_user(user, now));
        }
    }

    users.sort_by_key(|user| Reverse(user.hashrate));
    users.truncate(query.limit.unwrap_or(usize::MAX));

    Ok(Json(users).into_response())
}

async fn user(
    _: ApiAuth,
    State(state): State<UsersState>,
    Path(address): Path<Address<NetworkUnchecked>>,
) -> ServerResult<Response> {
    let address = address.assume_checked();

    let user = state
        .metatron
        .users()
        .get(&address)
        .ok_or_not_found(|| format!("User {address}"))?;

    Ok(Json(UserDetail::from_user(&user, Instant::now())).into_response())
}
