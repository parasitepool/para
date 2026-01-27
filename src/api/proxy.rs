use {super::*, crate::http_server, boilerplate::Boilerplate};

#[derive(Boilerplate)]
struct ProxyHtml;

pub(crate) fn router(metrics: Arc<Metrics>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/api/proxy/status", get(status))
        .route("/api/proxy/users", get(users))
        .route("/api/proxy/users/{address}", get(user))
        .route("/ws/logs", get(http_server::ws_logs))
        .route("/static/{*path}", get(http_server::static_assets))
        .with_state(metrics)
}

async fn home() -> Response {
    let html = ProxyHtml;

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
        hashrate_1m: metrics.metatron.hashrate_1m(),
        sps_1m: metrics.metatron.sps_1m(),
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
        sps_1m: user.sps_1m(),
        accepted: user.accepted(),
        rejected: user.rejected(),
        best_ever: user.best_ever(),
        total_work: user.total_work(),
        authorized: user.authorized,
        workers: user
            .workers()
            .map(|worker| WorkerDetail {
                name: worker.workername().to_string(),
                hashrate_1m: worker.hashrate_1m(),
                sps_1m: worker.sps_1m(),
                accepted: worker.accepted(),
                rejected: worker.rejected(),
                best_ever: worker.best_ever(),
                total_work: worker.total_work(),
            })
            .collect(),
    })
    .into_response())
}
