use {
    super::*,
    crate::http_server::auth::{ApiAuth, NavbarAuth},
    axum::extract::RawQuery,
};

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

    users.sort_by(|a, b| b.hashrate.total_cmp(&a.hashrate));
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
