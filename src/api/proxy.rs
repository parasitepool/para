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
    let orders = router.live_orders();

    let order = orders
        .first()
        .ok_or_not_found(|| "Proxy upstream".to_string())?;

    let upstream = order
        .upstream()
        .ok_or_not_found(|| "Proxy upstream".to_string())?;

    let stats = order.stats();
    let connected = usize::from(upstream.is_connected());

    Ok(Json(ProxyStatus {
        uptime_secs: metatron.uptime().as_secs(),
        block_count: metatron.block_count() as u64,
        recent_blocks: metatron.recent_blocks(10),
        upstream_info: UpstreamInfo::from_upstream(&upstream),
        upstream: UpstreamStatus {
            users: 1,
            workers: 1,
            orders: connected,
            pending: 0,
            disconnected: 1 - connected,
            hashrate_1m: stats.hashrate_1m(now),
            sps_1m: stats.sps_1m(now),
            accepted_shares: stats.accepted_shares,
            rejected_shares: stats.rejected_shares,
            accepted_work: stats.accepted_work,
            rejected_work: stats.rejected_work,
            total: UpstreamTotals {
                users: 1,
                orders: 1,
                stats: TotalStats::from_stats(&stats, now),
            },
        },
        downstream: DownstreamStatus::from_metatron(&metatron, now),
    })
    .into_response())
}
