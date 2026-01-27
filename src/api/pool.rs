use {
    super::*,
    axum::extract::ws::{Message, WebSocket, WebSocketUpgrade},
    boilerplate::Boilerplate,
    crate::{http_server, log_broadcast},
};

#[derive(Boilerplate)]
struct PoolHomeHtml;

pub(crate) fn router(metatron: Arc<Metatron>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/api/pool/status", get(status))
        .route("/api/pool/users", get(users))
        .route("/api/pool/users/{address}", get(user))
        .route("/ws/logs", get(logs_ws))
        .route("/static/{*path}", get(http_server::static_assets))
        .with_state(metatron)
}

async fn home() -> Response {
    let html = PoolHomeHtml;

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
        hashrate_1m: metatron.hash_rate_1m(),
        sps_1m: metatron.sps_1m(),
        users: metatron.total_users(),
        workers: metatron.total_workers(),
        connections: metatron.total_connections(),
        accepted: metatron.accepted(),
        rejected: metatron.rejected(),
        blocks: metatron.total_blocks(),
        best_ever: metatron.best_ever(),
        last_share: metatron.last_share().map(|time| time.elapsed().as_secs()),
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
        hashrate_1m: user.hash_rate_1m(),
        sps_1m: user.sps_1m(),
        accepted: user.accepted(),
        rejected: user.rejected(),
        best_ever: user.best_ever(),
        authorized: user.authorized,
        workers: user
            .workers()
            .map(|worker| WorkerDetail {
                name: worker.workername().to_string(),
                hashrate_1m: worker.hash_rate_1m(),
                sps_1m: worker.sps_1m(),
                accepted: worker.accepted(),
                rejected: worker.rejected(),
                best_ever: worker.best_ever(),
            })
            .collect(),
    })
    .into_response())
}

async fn logs_ws(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_logs_socket)
}

async fn handle_logs_socket(mut socket: WebSocket) {
    let Some(subscriber) = log_broadcast::subscriber() else {
        return;
    };

    for msg in subscriber.backlog() {
        if socket.send(Message::Text(msg.as_ref().into())).await.is_err() {
            return;
        }
    }

    let mut rx = subscriber.subscribe();

    while let Ok(msg) = rx.recv().await {
        if socket.send(Message::Text(msg.as_ref().into())).await.is_err() {
            break;
        }
    }
}
