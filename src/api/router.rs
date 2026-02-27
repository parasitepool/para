use super::*;

pub(crate) fn router(
    state: Arc<Router>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
) -> axum::Router {
    axum::Router::new()
        .route("/api/router/status", get(status))
        .route("/api/router/upstream/{upstream_id}", get(upstream))
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
    let mut total_accepted_work = TotalWork::ZERO;

    for (index, slot) in slots.iter().enumerate() {
        let stats = slot.metatron.snapshot();
        let hashrate_1m = stats.hashrate_1m(now);
        let sessions_count = slot.metatron.total_sessions();

        let mut session_hashrates = Vec::new();
        for user in slot.metatron.users().iter() {
            for worker in user.workers() {
                for session in worker.sessions() {
                    let session_stats = session.snapshot();
                    session_hashrates.push(session_stats.hashrate_1m(now));
                }
            }
        }

        session_hashrates.sort_by(|a, b| a.0.total_cmp(&b.0));

        let hashrate_min = session_hashrates.first().copied().unwrap_or(HashRate(0.0));
        let hashrate_max = session_hashrates.last().copied().unwrap_or(HashRate(0.0));

        let hashrate_avg = if session_hashrates.is_empty() {
            HashRate(0.0)
        } else {
            let sum: f64 = session_hashrates.iter().map(|h| h.0).sum();
            HashRate(sum / session_hashrates.len() as f64)
        };

        let hashrate_median = if session_hashrates.is_empty() {
            HashRate(0.0)
        } else {
            let mid = session_hashrates.len() / 2;
            if session_hashrates.len() % 2 == 0 {
                HashRate((session_hashrates[mid - 1].0 + session_hashrates[mid].0) / 2.0)
            } else {
                session_hashrates[mid]
            }
        };

        total_sessions += sessions_count;
        total_hashrate.0 += hashrate_1m.0;
        total_accepted_work += stats.accepted_work;

        slot_statuses.push(SlotStatus {
            index,
            upstream_id: slot.upstream.id(),
            endpoint: slot.upstream.endpoint().to_string(),
            username: slot.upstream.username().to_string(),
            connected: slot.upstream.is_connected(),
            hashrate_1m,
            ph_days: PhDays::from(stats.accepted_work),
            sessions: sessions_count,
            hashrate_min,
            hashrate_max,
            hashrate_avg,
            hashrate_median,
        });
    }

    Json(RouterStatus {
        slots: slot_statuses,
        total_sessions,
        total_upstreams: slots.len(),
        total_hashrate_1m: total_hashrate,
        total_ph_days: PhDays::from(total_accepted_work),
    })
}

async fn upstream(
    State(router): State<Arc<Router>>,
    Path(upstream_id): Path<u32>,
) -> ServerResult<Response> {
    let slot = router
        .slot_by_upstream_id(upstream_id)
        .ok_or_not_found(|| format!("Upstream {upstream_id}"))?;

    let now = Instant::now();
    let stats = slot.metatron.snapshot();

    let mut session_list = Vec::new();
    for user in slot.metatron.users().iter() {
        for worker in user.workers() {
            for session in worker.sessions() {
                let s = session.snapshot();
                session_list.push(SessionDetail {
                    id: session.id(),
                    upstream_id: session.id().upstream_id(),
                    address: session.address().to_string(),
                    workername: session.workername().to_string(),
                    username: session.username().to_string(),
                    enonce1: session.enonce1().clone(),
                    version_mask: session.version_mask(),
                    hashrate_1m: s.hashrate_1m(now),
                    hashrate_5m: s.hashrate_5m(now),
                    hashrate_15m: s.hashrate_15m(now),
                    hashrate_1hr: s.hashrate_1hr(now),
                    hashrate_6hr: s.hashrate_6hr(now),
                    hashrate_1d: s.hashrate_1d(now),
                    hashrate_7d: s.hashrate_7d(now),
                    sps_1m: s.sps_1m(now),
                    sps_5m: s.sps_5m(now),
                    sps_15m: s.sps_15m(now),
                    sps_1hr: s.sps_1hr(now),
                    accepted_shares: s.accepted_shares,
                    rejected_shares: s.rejected_shares,
                    best_ever: s.best_ever,
                    last_share: s.last_share.map(|time| now.duration_since(time).as_secs()),
                    accepted_work: s.accepted_work,
                    rejected_work: s.rejected_work,
                    ph_days: s.accepted_work.into(),
                });
            }
        }
    }

    Ok(Json(UpstreamDetail {
        upstream_id: slot.upstream.id(),
        endpoint: slot.upstream.endpoint().to_string(),
        username: slot.upstream.username().to_string(),
        connected: slot.upstream.is_connected(),
        ping_ms: slot.upstream.ping_ms(),
        difficulty: slot.upstream.difficulty(),
        enonce1: slot.upstream.enonce1().clone(),
        enonce2_size: slot.upstream.enonce2_size(),
        version_mask: slot.upstream.version_mask(),
        accepted: slot.upstream.accepted(),
        rejected: slot.upstream.rejected(),
        filtered: slot.upstream.filtered(),
        users: slot.metatron.total_users(),
        workers: slot.metatron.total_workers(),
        sessions: slot.metatron.total_sessions(),
        disconnected: slot.metatron.disconnected(),
        idle: slot.metatron.idle(),
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
        best_ever: stats.best_ever,
        last_share: stats
            .last_share
            .map(|time| now.duration_since(time).as_secs()),
        accepted_work: stats.accepted_work,
        rejected_work: stats.rejected_work,
        ph_days: stats.accepted_work.into(),
        uptime_secs: slot.metatron.uptime().as_secs(),
        session_list,
    })
    .into_response())
}
