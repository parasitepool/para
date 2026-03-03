use super::*;

pub(crate) fn router(
    metatron: Arc<Metatron>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
    logs: Arc<logs::Logs>,
) -> axum::Router {
    axum::Router::new()
        .route("/", get(home))
        .route("/users", get(users_page))
        .route("/user/{address}", get(user_page))
        .route("/api/pool/status", get(status))
        .route("/api/pool/users", get(super::users))
        .route("/api/pool/user/{address}", get(super::user))
        .with_state(metatron)
        .merge(http_server::common_routes())
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
}

async fn home(Extension(chain): Extension<Chain>) -> Response {
    render_page(PoolHtml, chain)
}

async fn users_page(Extension(chain): Extension<Chain>) -> Response {
    render_page(
        UsersHtml {
            title: "Pool | Users",
            api_base: "/api/pool",
        },
        chain,
    )
}

async fn user_page(Extension(chain): Extension<Chain>) -> Response {
    render_page(
        UserHtml {
            title: "Pool | User",
            api_base: "/api/pool",
        },
        chain,
    )
}

async fn status(State(metatron): State<Arc<Metatron>>) -> Json<PoolStatus> {
    let now = Instant::now();
    let stats = metatron.snapshot();

    Json(PoolStatus {
        endpoint: metatron.endpoint().to_string(),
        user_count: metatron.total_users(),
        worker_count: metatron.total_workers(),
        block_count: metatron.total_blocks(),
        session_count: metatron.total_sessions(),
        disconnected_count: metatron.total_disconnected(),
        idle_count: metatron.total_idle(),
        uptime_secs: metatron.uptime().as_secs(),
        stats: MiningStats::from_snapshot(&stats, now),
    })
}
