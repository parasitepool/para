use super::*;

pub(crate) fn router(
    metrics: Arc<Metrics>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
    logs: Arc<logs::Logs>,
) -> axum::Router {
    axum::Router::new()
        .route("/", get(home))
        .route("/users", get(users_page))
        .route("/user/{address}", get(user_page))
        .route("/api/proxy/status", get(status))
        .route("/api/proxy/users", get(users))
        .route("/api/proxy/user/{address}", get(user))
        .with_state(metrics)
        .merge(common_routes())
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
}

async fn users(State(metrics): State<Arc<Metrics>>) -> Json<Vec<String>> {
    pool::users(State(metrics.metatron.clone())).await
}

async fn user(
    State(metrics): State<Arc<Metrics>>,
    path: Path<Address<NetworkUnchecked>>,
) -> ServerResult<Response> {
    pool::user(State(metrics.metatron.clone()), path).await
}

async fn home(Extension(chain): Extension<Chain>) -> Response {
    render_page(ProxyHtml, chain)
}

async fn users_page(Extension(chain): Extension<Chain>) -> Response {
    render_page(
        UsersHtml {
            title: "Proxy | Users",
            api_base: "/api/proxy",
        },
        chain,
    )
}

async fn user_page(Extension(chain): Extension<Chain>) -> Response {
    render_page(
        UserHtml {
            title: "Proxy | User",
            api_base: "/api/proxy",
        },
        chain,
    )
}

async fn status(State(metrics): State<Arc<Metrics>>) -> Json<ProxyStatus> {
    Json(ProxyStatus {
        endpoint: metrics.metatron.endpoint().to_string(),
        user_count: metrics.metatron.total_users(),
        worker_count: metrics.metatron.total_workers(),
        session_count: metrics.metatron.total_sessions(),
        disconnected_count: metrics.metatron.total_disconnected(),
        idle_count: metrics.metatron.total_idle(),
        uptime_secs: metrics.metatron.uptime().as_secs(),
        upstream: UpstreamInfo::from_upstream(&metrics.upstream()),
        stats: MiningStats::from_snapshot(&metrics.metatron.snapshot(), Instant::now()),
    })
}
