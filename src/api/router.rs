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
        .with_state(state)
        .merge(http_server::common_routes())
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
}

async fn home(Extension(chain): Extension<Chain>) -> Response {
    render_page(RouterHtml, chain)
}

async fn upstream_page(Extension(chain): Extension<Chain>) -> Response {
    render_page(UpstreamHtml, chain)
}

async fn status(State(router): State<Arc<Router>>) -> Json<RouterStatus> {
    let now = Instant::now();
    let mut slots = Vec::new();
    let mut combined = Stats::new();
    let mut session_count = 0;
    let mut uptime_secs = 0;
    let mut upstream_accepted = 0u64;
    let mut upstream_rejected = 0u64;
    let mut upstream_accepted_work = TotalWork::ZERO;
    let mut upstream_rejected_work = TotalWork::ZERO;

    for slot in router.slots().iter() {
        let stats = slot.metatron.snapshot();

        let slot_session_count = slot.metatron.total_sessions();
        session_count += slot_session_count;
        uptime_secs = uptime_secs.max(slot.metatron.uptime().as_secs());

        let slot_upstream_accepted = slot.upstream.accepted();
        let slot_upstream_rejected = slot.upstream.rejected();
        let slot_upstream_accepted_work = slot.upstream.accepted_work();
        let slot_upstream_rejected_work = slot.upstream.rejected_work();

        slots.push(SlotStatus {
            upstream_id: slot.upstream.id(),
            endpoint: slot.upstream.endpoint().to_string(),
            username: slot.upstream.username().to_string(),
            ping_ms: slot.upstream.ping_ms(),
            upstream_accepted: slot_upstream_accepted,
            upstream_rejected: slot_upstream_rejected,
            upstream_accepted_work: slot_upstream_accepted_work,
            upstream_rejected_work: slot_upstream_rejected_work,
            upstream_ph_days: PhDays::from(
                slot_upstream_accepted_work + slot_upstream_rejected_work,
            ),
            session_count: slot_session_count,
            stats: MiningStats::from_snapshot(&stats, now),
        });

        upstream_accepted += slot_upstream_accepted;
        upstream_rejected += slot_upstream_rejected;
        upstream_accepted_work += slot_upstream_accepted_work;
        upstream_rejected_work += slot_upstream_rejected_work;

        combined.absorb(stats, now);
    }

    Json(RouterStatus {
        upstream_count: slots.len(),
        session_count,
        uptime_secs,
        slots,
        upstream_accepted,
        upstream_rejected,
        upstream_accepted_work,
        upstream_rejected_work,
        upstream_ph_days: (upstream_accepted_work + upstream_rejected_work).into(),
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
