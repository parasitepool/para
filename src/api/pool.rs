use super::*;

pub(crate) fn router(metatron: Arc<Metatron>, bitcoin_client: Arc<Client>, chain: Chain) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/api/pool/status", get(status))
        .route("/api/pool/users", get(users))
        .route("/api/pool/users/{address}", get(user))
        .route("/api/bitcoin/status", get(http_server::bitcoin_status))
        .route("/api/system/status", get(http_server::system_status))
        .route("/ws/logs", get(http_server::ws_logs))
        .route("/static/{*path}", get(http_server::static_assets))
        .with_state(metatron)
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
}

#[derive(Boilerplate)]
struct PoolHtml;

impl DashboardContent for PoolHtml {
    fn title(&self) -> &'static str {
        "Pool"
    }
}

async fn home(Extension(chain): Extension<Chain>) -> Response {
    let html = DashboardHtml::new(PoolHtml, chain);

    #[cfg(feature = "reload")]
    let body = match html.reload_from_path() {
        Ok(reloaded) => reloaded.to_string(),
        Err(_) => html.to_string(),
    };

    #[cfg(not(feature = "reload"))]
    let body = html.to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}

async fn status(State(metatron): State<Arc<Metatron>>) -> Json<PoolStatus> {
    Json(PoolStatus {
        endpoint: metatron.endpoint().to_string(),
        hashrate_1m: metatron.hashrate_1m(),
        hashrate_5m: metatron.hashrate_5m(),
        hashrate_15m: metatron.hashrate_15m(),
        hashrate_1hr: metatron.hashrate_1hr(),
        hashrate_6hr: metatron.hashrate_6hr(),
        hashrate_1d: metatron.hashrate_1d(),
        hashrate_7d: metatron.hashrate_7d(),
        sps_1m: metatron.sps_1m(),
        sps_5m: metatron.sps_5m(),
        sps_15m: metatron.sps_15m(),
        sps_1hr: metatron.sps_1hr(),
        users: metatron.total_users(),
        workers: metatron.total_workers(),
        connections: metatron.total_connections(),
        disconnected: metatron.disconnected(),
        idle: metatron.idle(),
        accepted: metatron.accepted(),
        rejected: metatron.rejected(),
        blocks: metatron.total_blocks(),
        best_ever: metatron.best_ever(),
        last_share: metatron.last_share().map(|time| time.elapsed().as_secs()),
        total_work: metatron.total_work(),
        uptime_secs: metatron.uptime().as_secs(),
    })
}

async fn users(State(metatron): State<Arc<Metatron>>) -> Json<Vec<String>> {
    Json(
        metatron
            .users()
            .iter()
            .map(|entry| entry.key().to_string())
            .collect(),
    )
}

async fn user(
    State(metatron): State<Arc<Metatron>>,
    Path(address): Path<Address<NetworkUnchecked>>,
) -> ServerResult<Response> {
    let address = address.assume_checked();

    let user = metatron
        .users()
        .get(&address)
        .ok_or_not_found(|| format!("User {address}"))?;

    Ok(Json(UserDetail {
        address: user.address.to_string(),
        hashrate_1m: user.hashrate_1m(),
        hashrate_5m: user.hashrate_5m(),
        hashrate_15m: user.hashrate_15m(),
        hashrate_1hr: user.hashrate_1hr(),
        hashrate_6hr: user.hashrate_6hr(),
        hashrate_1d: user.hashrate_1d(),
        hashrate_7d: user.hashrate_7d(),
        sps_1m: user.sps_1m(),
        sps_5m: user.sps_5m(),
        sps_15m: user.sps_15m(),
        sps_1hr: user.sps_1hr(),
        accepted: user.accepted(),
        rejected: user.rejected(),
        best_ever: user.best_ever(),
        last_share: user.last_share().map(|time| time.elapsed().as_secs()),
        total_work: user.total_work(),
        authorized: user.authorized,
        workers: user
            .workers()
            .map(|worker| WorkerDetail {
                name: worker.workername().to_string(),
                hashrate_1m: worker.hashrate_1m(),
                hashrate_5m: worker.hashrate_5m(),
                hashrate_15m: worker.hashrate_15m(),
                hashrate_1hr: worker.hashrate_1hr(),
                hashrate_6hr: worker.hashrate_6hr(),
                hashrate_1d: worker.hashrate_1d(),
                hashrate_7d: worker.hashrate_7d(),
                sps_1m: worker.sps_1m(),
                sps_5m: worker.sps_5m(),
                sps_15m: worker.sps_15m(),
                sps_1hr: worker.sps_1hr(),
                accepted: worker.accepted(),
                rejected: worker.rejected(),
                best_ever: worker.best_ever(),
                last_share: worker.last_share().map(|time| time.elapsed().as_secs()),
                total_work: worker.total_work(),
            })
            .collect(),
    })
    .into_response())
}
