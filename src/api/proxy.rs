use {
    super::*,
    crate::{
        http_server::auth::{BearerAuth, NavbarAuth},
        router::Router,
    },
};

pub(crate) fn router(
    router: Arc<Router>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
    logs: Arc<logs::Logs>,
    http_api_token: Option<&str>,
    http_admin_token: Option<&str>,
) -> axum::Router {
    let metatron = router.metatron();

    axum::Router::new()
        .route("/", get(home))
        .route("/api/proxy/status", get(status))
        .with_state(router)
        .merge(users::routes(users::Service::Proxy, metatron))
        .merge(common_routes())
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
        .layer(Extension(BearerAuth::new(http_api_token, http_admin_token)))
}

async fn home(Extension(chain): Extension<Chain>, auth: NavbarAuth) -> Response {
    render_page(ProxyHtml, chain, auth)
}

async fn status(State(router): State<Arc<Router>>) -> ServerResult<Response> {
    let now = Instant::now();
    let metatron = router.metatron();
    let orders = router.orders();

    let order = orders
        .first()
        .ok_or_not_found(|| "Proxy upstream".to_string())?;

    let upstream = order
        .upstream()
        .ok_or_not_found(|| "Proxy upstream".to_string())?;

    Ok(Json(ProxyStatus {
        uptime_secs: metatron.uptime().as_secs(),
        block_count: metatron.block_count() as u64,
        last_block_hash: metatron.last_block().map(|h| h.to_string()),
        upstream: UpstreamInfo::from_upstream(&upstream, &metatron, now),
        downstream: DownstreamInfo::from_metatron(&metatron, now),
    })
    .into_response())
}
