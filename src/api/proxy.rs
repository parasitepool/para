use super::*;

pub(crate) fn router(
    metrics: Arc<Metrics>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
    logs: Arc<logs::Logs>,
) -> axum::Router {
    let user_routes = axum::Router::new()
        .route("/api/proxy/users", get(super::users))
        .route("/api/proxy/user/{address}", get(super::user))
        .with_state(metrics.metatron.clone());

    axum::Router::new()
        .route("/", get(home))
        .route("/users", get(users_page))
        .route("/user/{address}", get(user_page))
        .route("/api/proxy/status", get(status))
        .with_state(metrics)
        .merge(user_routes)
        .merge(common_routes())
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
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
