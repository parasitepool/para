use super::*;

pub(crate) fn router(
    state: Arc<Router>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
) -> axum::Router {
    axum::Router::new()
        .route("/api/router/status", get(status))
        .route("/api/bitcoin/status", get(http_server::bitcoin_status))
        .route("/api/system/status", get(http_server::system_status))
        .with_state(state)
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
}

async fn status(State(router): State<Arc<Router>>) -> Json<RouterStatus> {
    let now = Instant::now();

    let slots = router.slots();
    let mut slot_statuses = Vec::with_capacity(slots.len());
    let mut total_sessions = 0;
    let mut total_hashrate = HashRate(0.0);

    for (index, slot) in slots.iter().enumerate() {
        let stats = slot.metatron.snapshot();
        let hashrate_1m = stats.hashrate_1m(now);

        let connected = slot.upstream.is_connected();

        let mut sessions = Vec::new();
        for user in slot.metatron.users().iter() {
            for worker in user.workers() {
                for session in worker.sessions() {
                    let session_stats = session.snapshot();
                    sessions.push(SlotSessionStatus {
                        id: session.id(),
                        worker_name: session.workername().to_string(),
                        hashrate_1m: session_stats.hashrate_1m(now),
                    });
                }
            }
        }

        total_sessions += sessions.len();
        total_hashrate.0 += hashrate_1m.0;

        slot_statuses.push(SlotStatus {
            index,
            endpoint: slot.upstream.endpoint().to_string(),
            username: slot.upstream.username().to_string(),
            connected,
            hashrate_1m,
            sessions,
        });
    }

    Json(RouterStatus {
        slots: slot_statuses,
        total_sessions,
        total_hashrate_1m: total_hashrate,
    })
}
