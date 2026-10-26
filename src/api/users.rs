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
    pub remote_address: IpAddr,
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
            remote_address: session.socket_addr().ip(),
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
            &format!("{}/users/count", service.api_base()),
            get(count_users),
        )
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

#[derive(Copy, Clone, Default, PartialEq, Eq)]
enum UserSort {
    #[default]
    Hashrate,
    Address,
}

#[derive(Default)]
struct UsersQuery {
    search: Option<String>,
    sort: UserSort,
    cursor: Option<String>,
    limit: Option<usize>,
}

impl UsersQuery {
    fn parse(raw: Option<&str>) -> ServerResult<Self> {
        let mut query = Self::default();

        for (key, value) in parse_query_pairs(raw)? {
            match key.as_str() {
                "search" if !value.trim().is_empty() => {
                    query.search = Some(value.trim().to_lowercase());
                }
                "search" => query.search = None,
                "sort" if !value.trim().is_empty() => {
                    query.sort = match value.as_str() {
                        "hashrate" => UserSort::Hashrate,
                        "address" => UserSort::Address,
                        _ => {
                            return Err(ServerError::BadRequest(format!(
                                "invalid user sort `{value}`"
                            )));
                        }
                    };
                }
                "sort" => query.sort = UserSort::Hashrate,
                "cursor" if !value.trim().is_empty() => {
                    let address = value.parse::<Address<NetworkUnchecked>>().map_err(|err| {
                        ServerError::BadRequest(format!("invalid address cursor `{value}`: {err}"))
                    })?;
                    query.cursor = Some(address.assume_checked().to_string());
                }
                "cursor" => query.cursor = None,
                "limit" if !value.trim().is_empty() => {
                    query.limit = Some(parse_usize_query_param("limit", &value)?);
                }
                "limit" => query.limit = None,
                _ => {}
            }
        }

        if query.sort != UserSort::Address && query.cursor.is_some() {
            return Err(ServerError::BadRequest(
                "address cursor requires sort=address".into(),
            ));
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
    let limit = effective_limit(query.limit)?;

    let mut users = Vec::new();

    for entry in state.metatron.users().iter() {
        let user = entry.value();

        if query.matches(user) {
            users.push(UserSummary::from_user(user, now));
        }
    }

    let address = |user: &UserSummary| user.address.clone().assume_checked().to_string();

    match query.sort {
        UserSort::Hashrate => {
            users.sort_by_cached_key(|user| (Reverse(user.hashrate), address(user)));
        }
        UserSort::Address => {
            users.sort_by_cached_key(address);

            if let Some(cursor) = &query.cursor {
                let start = users.partition_point(|user| address(user).as_str() <= cursor.as_str());
                users.drain(..start);
            }
        }
    }

    let has_more = users.len() > limit;
    users.truncate(limit);

    let next_cursor = (query.sort == UserSort::Address && has_more)
        .then(|| users.last().map(address))
        .flatten();

    Ok(page_response(users, limit, next_cursor))
}

async fn count_users(
    _: ApiAuth,
    State(state): State<UsersState>,
    RawQuery(raw_query): RawQuery,
) -> ServerResult<Response> {
    let query = UsersQuery::parse(raw_query.as_deref())?;

    if query.cursor.is_some() {
        return Err(ServerError::BadRequest(
            "cursor is not supported for user counts".into(),
        ));
    }

    let count = state
        .metatron
        .users()
        .iter()
        .filter(|entry| query.matches(entry.value()))
        .count() as u64;

    Ok(Json(CountResponse { count }).into_response())
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

#[cfg(test)]
mod tests {
    use {
        super::*,
        axum::{body::to_bytes, extract::State},
    };

    fn address(value: &str) -> Address {
        value
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    async fn request_page(state: &UsersState, query: &str) -> Response {
        users(
            ApiAuth,
            State(state.clone()),
            RawQuery(Some(query.to_string())),
        )
        .await
        .ok()
        .expect("users handler failed")
    }

    async fn decode_page(response: Response) -> Vec<UserSummary> {
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn page(state: &UsersState, query: &str) -> Vec<UserSummary> {
        decode_page(request_page(state, query).await).await
    }

    async fn count(state: &UsersState, query: &str) -> u64 {
        let response = count_users(
            ApiAuth,
            State(state.clone()),
            RawQuery(Some(query.to_string())),
        )
        .await
        .ok()
        .expect("count_users handler failed");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        value["count"].as_u64().unwrap()
    }

    fn addresses(users: &[UserSummary]) -> Vec<String> {
        users
            .iter()
            .map(|user| user.address.clone().assume_checked().to_string())
            .collect()
    }

    #[test]
    fn address_cursor_requires_address_sort() {
        assert!(
            UsersQuery::parse(Some("cursor=tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc")).is_err()
        );
        assert!(
            UsersQuery::parse(Some(
                "sort=address&cursor=tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            ))
            .is_ok()
        );
        assert!(UsersQuery::parse(Some("sort=unknown")).is_err());
    }

    #[tokio::test]
    async fn address_sort_paginates_all_users() {
        let (metatron, _directory) = Metatron::test();
        let metatron = Arc::new(metatron);

        let expected = [
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc",
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
            "tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw",
        ]
        .map(address);

        for address in &expected {
            metatron
                .users()
                .insert(address.clone(), Arc::new(User::new(address.clone())));
        }

        let mut expected = expected.iter().map(ToString::to_string).collect::<Vec<_>>();
        expected.sort();

        let state = UsersState {
            service: Service::Router,
            metatron,
        };

        let first_response = request_page(&state, "sort=address&limit=2").await;
        assert_eq!(first_response.headers()["x-limit"], "2");
        assert_eq!(first_response.headers()["x-next-cursor"], expected[1]);
        let first = decode_page(first_response).await;
        assert_eq!(addresses(&first), expected[..2]);

        let second_response = request_page(
            &state,
            &format!("sort=address&limit=2&cursor={}", expected[1]),
        )
        .await;
        assert!(!second_response.headers().contains_key("x-next-cursor"));
        let second = decode_page(second_response).await;
        assert_eq!(addresses(&second), expected[2..]);

        let after_last = page(
            &state,
            &format!("sort=address&limit=2&cursor={}", expected[2]),
        )
        .await;
        assert!(after_last.is_empty());

        let narrowed_address = address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx").to_string();

        let narrowed = page(&state, "search=w508&sort=address&limit=2").await;
        assert_eq!(addresses(&narrowed), vec![narrowed_address.clone()]);

        let narrowed_after = page(
            &state,
            &format!("search=w508&sort=address&cursor={narrowed_address}"),
        )
        .await;
        assert!(narrowed_after.is_empty());

        let top_by_hashrate = request_page(&state, "limit=2").await;
        assert!(!top_by_hashrate.headers().contains_key("x-next-cursor"));
        assert_eq!(decode_page(top_by_hashrate).await.len(), 2);

        let clamped = request_page(&state, "limit=999").await;
        assert_eq!(clamped.headers()["x-limit"], "500");

        assert_eq!(count(&state, "").await, 3);
        assert_eq!(count(&state, "search=w508").await, 1);

        let count_with_cursor = count_users(
            ApiAuth,
            State(state.clone()),
            RawQuery(Some(format!("sort=address&cursor={}", expected[0]))),
        )
        .await;
        assert!(count_with_cursor.is_err());
    }
}
