use {super::*, crate::subcommand::router::RouterSlots};

pub(crate) fn router(
    slots: RouterSlots,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
) -> Router {
    Router::new()
        .route("/api/router/status", get(status))
        .route("/api/bitcoin/status", get(http_server::bitcoin_status))
        .route("/api/system/status", get(http_server::system_status))
        .with_state(slots)
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
}

async fn status(State(slots): State<RouterSlots>) -> Json<RouterStatus> {
    let now = Instant::now();

    let mut upstreams = Vec::with_capacity(slots.0.len());
    let mut total_sessions = 0;
    let mut total_hashrate = HashRate(0.0);

    for (index, slot) in slots.0.iter().enumerate() {
        let stats = slot.metatron.snapshot();
        let hashrate_1m = stats.hashrate_1m(now);

        let connected = slot
            .state
            .read()
            .as_ref()
            .map(|active| active.upstream.is_connected())
            .unwrap_or(false);

        let mut sessions = Vec::new();
        for user in slot.metatron.users().iter() {
            for worker in user.workers() {
                for session in worker.sessions() {
                    let session_stats = session.snapshot();
                    sessions.push(UpstreamSessionStatus {
                        id: session.id(),
                        worker_name: session.workername().to_string(),
                        hashrate_1m: session_stats.hashrate_1m(now),
                    });
                }
            }
        }

        total_sessions += sessions.len();
        total_hashrate.0 += hashrate_1m.0;

        upstreams.push(UpstreamStatus {
            index,
            endpoint: slot.target.endpoint.clone(),
            username: slot.target.username.to_string(),
            connected,
            hashrate_1m,
            sessions,
        });
    }

    Json(RouterStatus {
        upstreams,
        total_sessions,
        total_hashrate_1m: total_hashrate,
    })
}
