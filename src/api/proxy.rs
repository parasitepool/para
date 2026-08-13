use {
    super::*,
    crate::{
        http_server::auth::{BearerAuth, NavbarAuth},
        router::Router,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyStatus {
    pub uptime_secs: u64,
    pub block_count: u64,
    pub recent_blocks: Vec<BlockHash>,
    pub upstream_info: UpstreamInfo,
    pub upstream: UpstreamStats,
    pub downstream: DownstreamStats,
    pub git_commit: String,
}

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
        upstream: UpstreamStats {
            users: 1,
            workers: 1,
            orders: connected,
            pending: 0,
            disconnected: 1 - connected,
            hashrate_1m: stats.hashrate_1m(now),
            hashrate_5m: stats.hashrate_5m(now),
            hashrate_15m: stats.hashrate_15m(now),
            hashrate_1hr: stats.hashrate_1hr(now),
            hashrate_6hr: stats.hashrate_6hr(now),
            hashrate_1d: stats.hashrate_1d(now),
            hashrate_7d: stats.hashrate_7d(now),
            sps_1m: stats.sps_1m(now),
            sps_5m: stats.sps_5m(now),
            sps_15m: stats.sps_15m(now),
            sps_1hr: stats.sps_1hr(now),
            accepted_shares: stats.accepted_shares,
            rejected_shares: stats.rejected_shares,
            accepted_work: stats.accepted_work,
            rejected_work: stats.rejected_work,
            totals: UpstreamTotals {
                users: 1,
                orders: 1,
                accepted_shares: stats.accepted_shares,
                rejected_shares: stats.rejected_shares,
                accepted_work: stats.accepted_work,
                rejected_work: stats.rejected_work,
                delivered_hash_days: stats.delivered_work().to_hash_days(),
                best_share: stats.best_share,
                last_share: stats.last_share_epoch_secs(now),
            },
        },
        downstream: DownstreamStats::from_metatron(&metatron, now),
        git_commit: env!("GIT_COMMIT").into(),
    })
    .into_response())
}
