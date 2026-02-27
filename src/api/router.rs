use super::*;

pub(crate) fn router(
    state: Arc<Router>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
    logs: Arc<logs::Logs>,
) -> axum::Router {
    axum::Router::new()
        .route("/", get(home))
        .route("/upstream/{upstream_id}", get(upstream_page))
        .route("/api/router/status", get(status))
        .route("/api/router/upstream/{upstream_id}", get(upstream))
        .route("/api/bitcoin/status", get(http_server::bitcoin_status))
        .route("/api/system/status", get(http_server::system_status))
        .route("/ws/logs", get(http_server::ws_logs))
        .route("/static/{*path}", get(http_server::static_assets))
        .with_state(state)
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
}

async fn home(Extension(chain): Extension<Chain>) -> Response {
    #[cfg(feature = "reload")]
    let body = {
        use http_server::templates::ReloadedContent;

        let content = RouterHtml
            .reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| RouterHtml.to_string());

        let html = DashboardHtml::new(
            ReloadedContent {
                html: content,
                title: "Router",
            },
            chain,
        );

        html.reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| html.to_string())
    };

    #[cfg(not(feature = "reload"))]
    let body = DashboardHtml::new(RouterHtml, chain).to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}

async fn upstream_page(Extension(chain): Extension<Chain>) -> Response {
    #[cfg(feature = "reload")]
    let body = {
        use http_server::templates::ReloadedContent;

        let content = UpstreamHtml
            .reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| UpstreamHtml.to_string());

        let html = DashboardHtml::new(
            ReloadedContent {
                html: content,
                title: "Router | Upstream",
            },
            chain,
        );

        html.reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| html.to_string())
    };

    #[cfg(not(feature = "reload"))]
    let body = DashboardHtml::new(UpstreamHtml, chain).to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}

async fn status(State(router): State<Arc<Router>>) -> Json<RouterStatus> {
    let now = Instant::now();
    let mut slots = Vec::new();
    let mut session_count = 0;
    let mut total_hashrate = HashRate(0.0);
    let mut total_sps_1m = 0.0;
    let mut accepted_shares = 0;
    let mut rejected_shares = 0;
    let mut total_accepted_work = TotalWork::ZERO;
    let mut total_rejected_work = TotalWork::ZERO;
    let mut best_ever = None;
    let mut last_share = None;
    let mut uptime_secs = 0;

    for slot in router.slots().iter() {
        let stats = slot.metatron.snapshot();

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

        let hashrate_1m = stats.hashrate_1m(now);
        let slot_session_count = slot.metatron.session_count();

        session_count += slot_session_count;
        total_hashrate += hashrate_1m;
        total_sps_1m += stats.sps_1m(now);
        accepted_shares += stats.accepted_shares;
        rejected_shares += stats.rejected_shares;
        total_accepted_work += stats.accepted_work;
        total_rejected_work += stats.rejected_work;
        uptime_secs = uptime_secs.max(slot.metatron.uptime().as_secs());

        if stats
            .best_ever
            .is_some_and(|slot_best_ever| best_ever.is_none_or(|current| slot_best_ever > current))
        {
            best_ever = stats.best_ever;
        }

        if stats.last_share.is_some_and(|slot_last_share| {
            last_share.is_none_or(|current| slot_last_share > current)
        }) {
            last_share = stats.last_share;
        }

        slots.push(SlotStatus {
            upstream_id: slot.upstream.id(),
            endpoint: slot.upstream.endpoint().to_string(),
            username: slot.upstream.username().to_string(),
            hashrate_1m,
            ph_days: PhDays::from(stats.accepted_work),
            session_count: slot_session_count,
            hashrate_min,
            hashrate_max,
            hashrate_avg,
            hashrate_median,
        });
    }

    Json(RouterStatus {
        upstream_count: slots.len(),
        session_count,
        hashrate_1m: total_hashrate,
        ph_days: PhDays::from(total_accepted_work),
        sps_1m: total_sps_1m,
        accepted_shares,
        rejected_shares,
        best_ever,
        last_share: last_share.map(|time| now.duration_since(time).as_secs()),
        accepted_work: total_accepted_work,
        rejected_work: total_rejected_work,
        uptime_secs,
        slots,
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

    let mut sessions = Vec::new();
    for user in slot.metatron.users().iter() {
        for worker in user.workers() {
            for session in worker.sessions() {
                let s = session.snapshot();
                sessions.push(SessionDetail {
                    id: session.id(),
                    upstream_id: session.id().upstream_id(),
                    address: session.address().to_string(),
                    workername: session.workername().to_string(),
                    username: session.username().to_string(),
                    enonce1: session.enonce1().clone(),
                    version_mask: session.version_mask(),
                    accepted_shares: s.accepted_shares,
                    rejected_shares: s.rejected_shares,
                    accepted_work: s.accepted_work,
                    rejected_work: s.rejected_work,
                    best_ever: s.best_ever,
                    last_share: s.last_share.map(|time| now.duration_since(time).as_secs()),
                    ph_days: s.accepted_work.into(),
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
        session_count: slot.metatron.session_count(),
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
        sessions,
    })
    .into_response())
}
