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
        .merge(common_routes())
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
    let mut upstream_accepted = 0;
    let mut upstream_rejected = 0;
    let mut upstream_accepted_work = TotalWork::ZERO;
    let mut upstream_rejected_work = TotalWork::ZERO;
    let metatron = router.metatron();

    for slot in router.slots().iter() {
        let slot_upstream_accepted = slot.upstream.accepted();
        let slot_upstream_rejected = slot.upstream.rejected();
        let slot_upstream_accepted_work = slot.upstream.accepted_work();
        let slot_upstream_rejected_work = slot.upstream.rejected_work();
        let upstream_id = slot.upstream.id();

        slots.push(SlotStatus {
            upstream_id,
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
            session_count: metatron.upstream_session_count(upstream_id),
            disconnected_count: metatron.upstream_disconnected_count(upstream_id),
            idle_count: metatron.upstream_idle_count(upstream_id),
            stats: MiningStats::from_snapshot(&metatron.upstream_snapshot(upstream_id), now),
        });

        upstream_accepted += slot_upstream_accepted;
        upstream_rejected += slot_upstream_rejected;
        upstream_accepted_work += slot_upstream_accepted_work;
        upstream_rejected_work += slot_upstream_rejected_work;
    }

    Json(RouterStatus {
        upstream_count: slots.len(),
        session_count: metatron.total_sessions(),
        disconnected_count: metatron.total_disconnected(),
        idle_count: metatron.total_idle(),
        uptime_secs: metatron.uptime().as_secs(),
        slots,
        upstream_accepted,
        upstream_rejected,
        upstream_accepted_work,
        upstream_rejected_work,
        upstream_ph_days: (upstream_accepted_work + upstream_rejected_work).into(),
        stats: MiningStats::from_snapshot(&metatron.snapshot(), now),
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
    let metatron = router.metatron();
    let id = slot.upstream.id();
    let sessions = metatron.upstream_sessions(id);
    let session_details = sessions
        .iter()
        .map(|session| SessionDetail::from_session(session, now))
        .collect();
    let workers = metatron
        .users()
        .iter()
        .flat_map(|user| user.workers().collect::<Vec<_>>())
        .filter(|worker| worker.upstream_session_count(id) > 0)
        .map(|worker| WorkerDetail {
            name: worker.workername().to_string(),
            session_count: worker.upstream_session_count(id),
            stats: MiningStats::from_snapshot(&worker.upstream_snapshot(id), now),
        })
        .collect();

    Ok(Json(UpstreamDetail {
        upstream_id: id,
        upstream: UpstreamInfo::from_upstream(&slot.upstream),
        user_count: metatron.upstream_user_count(id),
        worker_count: metatron.upstream_worker_count(id),
        session_count: metatron.upstream_session_count(id),
        disconnected_count: metatron.upstream_disconnected_count(id),
        idle_count: metatron.upstream_idle_count(id),
        uptime_secs: metatron.uptime().as_secs(),
        workers,
        sessions: session_details,
        stats: MiningStats::from_snapshot(&metatron.upstream_snapshot(id), now),
    })
    .into_response())
}
