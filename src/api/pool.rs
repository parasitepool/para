use {
    super::*,
    crate::http_server::auth::{BearerAuth, NavbarAuth},
};

pub(crate) fn router(
    metatron: Arc<Metatron>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
    logs: Arc<logs::Logs>,
    http_api_token: Option<&str>,
    http_admin_token: Option<&str>,
) -> axum::Router {
    axum::Router::new()
        .route("/", get(home))
        .route("/api/pool/status", get(status))
        .with_state(metatron.clone())
        .merge(users::routes(users::Service::Pool, metatron))
        .merge(common_routes())
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
        .layer(Extension(BearerAuth::new(http_api_token, http_admin_token)))
}

async fn home(Extension(chain): Extension<Chain>, auth: NavbarAuth) -> Response {
    render_page(PoolHtml, chain, auth)
}

async fn status(State(metatron): State<Arc<Metatron>>) -> Json<PoolStatus> {
    Json(PoolStatus {
        block_count: metatron.block_count() as u64,
        recent_blocks: metatron.recent_blocks(10),
        uptime_secs: metatron.uptime().as_secs(),
        downstream: DownstreamInfo::from_metatron(&metatron, Instant::now()),
    })
}
