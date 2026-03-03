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
    let mut combined = Stats::new();
    let mut session_count = 0;
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

        let slot_session_count = slot.metatron.total_sessions();
        session_count += slot_session_count;
        uptime_secs = uptime_secs.max(slot.metatron.uptime().as_secs());

        slots.push(SlotStatus {
            upstream_id: slot.upstream.id(),
            endpoint: slot.upstream.endpoint().to_string(),
            username: slot.upstream.username().to_string(),
            hashrate_1m: stats.hashrate_1m(now),
            ph_days: PhDays::from(stats.accepted_work),
            upstream_ph_days: PhDays::from(
                slot.upstream.accepted_work() + slot.upstream.rejected_work(),
            ),
            session_count: slot_session_count,
            hashrate_min,
            hashrate_max,
            hashrate_avg,
            hashrate_median,
        });

        combined.absorb(stats, now);
    }

    Json(RouterStatus {
        upstream_count: slots.len(),
        session_count,
        uptime_secs,
        slots,
        stats: MiningStats::from_snapshot(&combined, now),
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
    let mut workers = Vec::new();
    for user in slot.metatron.users().iter() {
        for worker in user.workers() {
            let worker_stats = worker.snapshot();
            workers.push(WorkerDetail {
                name: worker.workername().to_string(),
                session_count: worker.session_count(),
                stats: MiningStats::from_snapshot(&worker_stats, now),
            });
            for session in worker.sessions() {
                let s = session.snapshot();
                sessions.push(SessionDetail {
                    id: session.id(),
                    upstream_id: session.id().upstream_id(),
                    address: session.address().to_string(),
                    worker_name: session.workername().to_string(),
                    username: session.username().to_string(),
                    enonce1: session.enonce1().clone(),
                    version_mask: session.version_mask(),
                    stats: MiningStats::from_snapshot(&s, now),
                });
            }
        }
    }

    Ok(Json(UpstreamDetail {
        upstream_id: slot.upstream.id(),
        upstream: UpstreamInfo::from_upstream(&slot.upstream),
        user_count: slot.metatron.total_users(),
        worker_count: slot.metatron.total_workers(),
        session_count: slot.metatron.total_sessions(),
        disconnected_count: slot.metatron.total_disconnected(),
        idle_count: slot.metatron.total_idle(),
        uptime_secs: slot.metatron.uptime().as_secs(),
        workers,
        sessions,
        stats: MiningStats::from_snapshot(&stats, now),
    })
    .into_response())
}
