use super::*;

pub(crate) fn router(metrics: Arc<Metrics>, bitcoin_client: Arc<Client>, chain: Chain) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/api/proxy/status", get(status))
        .route("/api/proxy/users", get(users))
        .route("/api/proxy/users/{address}", get(user))
        .route("/api/bitcoin/status", get(http_server::bitcoin_status))
        .route("/api/system/status", get(http_server::system_status))
        .route("/ws/logs", get(http_server::ws_logs))
        .route("/static/{*path}", get(http_server::static_assets))
        .with_state(metrics)
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
}

async fn home(Extension(chain): Extension<Chain>) -> Response {
    let html = DashboardHtml::new(ProxyHtml, chain);

    #[cfg(feature = "reload")]
    let body = match html.reload_from_path() {
        Ok(reloaded) => reloaded.to_string(),
        Err(_) => html.to_string(),
    };

    #[cfg(not(feature = "reload"))]
    let body = html.to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}

async fn status(State(metrics): State<Arc<Metrics>>) -> Json<ProxyStatus> {
    Json(ProxyStatus {
        endpoint: metrics.metatron.endpoint().to_string(),
        hashrate_1m: metrics.metatron.hashrate_1m(),
        hashrate_5m: metrics.metatron.hashrate_5m(),
        hashrate_15m: metrics.metatron.hashrate_15m(),
        hashrate_1hr: metrics.metatron.hashrate_1hr(),
        hashrate_6hr: metrics.metatron.hashrate_6hr(),
        hashrate_1d: metrics.metatron.hashrate_1d(),
        hashrate_7d: metrics.metatron.hashrate_7d(),
        sps_1m: metrics.metatron.sps_1m(),
        sps_5m: metrics.metatron.sps_5m(),
        sps_15m: metrics.metatron.sps_15m(),
        sps_1hr: metrics.metatron.sps_1hr(),
        users: metrics.metatron.total_users(),
        workers: metrics.metatron.total_workers(),
        connections: metrics.metatron.total_connections(),
        disconnected: metrics.metatron.disconnected(),
        idle: metrics.metatron.idle(),
        accepted: metrics.metatron.accepted(),
        rejected: metrics.metatron.rejected(),
        best_ever: metrics.metatron.best_ever(),
        last_share: metrics
            .metatron
            .last_share()
            .map(|time| time.elapsed().as_secs()),
        total_work: metrics.metatron.total_work(),
        uptime_secs: metrics.metatron.uptime().as_secs(),
        upstream_endpoint: metrics.upstream.endpoint().to_string(),
        upstream_difficulty: metrics.upstream.difficulty().await.as_f64(),
        upstream_username: metrics.upstream.username().clone(),
        upstream_connected: metrics.upstream.is_connected(),
        upstream_enonce1: metrics.upstream.enonce1().clone(),
        upstream_enonce2_size: metrics.upstream.enonce2_size(),
        upstream_version_mask: metrics.upstream.version_mask(),
        upstream_accepted: metrics.upstream.accepted(),
        upstream_rejected: metrics.upstream.rejected(),
    })
}

async fn users(State(metrics): State<Arc<Metrics>>) -> Json<Vec<String>> {
    Json(
        metrics
            .metatron
            .users()
            .iter()
            .map(|entry| entry.key().to_string())
            .collect(),
    )
}

async fn user(
    State(metrics): State<Arc<Metrics>>,
    Path(address): Path<Address<NetworkUnchecked>>,
) -> ServerResult<Response> {
    let address = address.assume_checked();

    let user = metrics
        .metatron
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
