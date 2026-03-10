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
            session_count: 0,
            disconnected_count: 0,
            idle_count: 0,
            stats: MiningStats::from_snapshot(&Stats::new(), now),
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

    Ok(Json(UpstreamDetail {
        upstream_id: slot.upstream.id(),
        upstream: UpstreamInfo::from_upstream(&slot.upstream),
        user_count: 0,
        worker_count: 0,
        session_count: 0,
        disconnected_count: 0,
        idle_count: 0,
        uptime_secs: 0,
        workers: Vec::new(),
        sessions: Vec::new(),
        stats: MiningStats::from_snapshot(&Stats::new(), now),
    })
    .into_response())
}
