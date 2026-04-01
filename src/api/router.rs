use super::*;

pub(crate) fn router(
    state: Arc<Router>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
    logs: Arc<logs::Logs>,
) -> axum::Router {
    axum::Router::new()
        .route("/", get(home))
        .route("/order/{id}", get(order_page))
        .route("/api/router/status", get(status))
        .route("/api/router/order", post(add_order))
        .route("/api/router/order/{id}", get(order_detail))
        .route("/api/router/order/{id}", delete(remove_order))
        .with_state(state)
        .merge(common_routes())
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
}

async fn home(Extension(chain): Extension<Chain>) -> Response {
    render_page(RouterHtml, chain)
}

async fn order_page(Extension(chain): Extension<Chain>) -> Response {
    render_page(OrderHtml, chain)
}

async fn status(State(router): State<Arc<Router>>) -> Json<RouterStatus> {
    let now = Instant::now();
    let mut orders = Vec::new();
    let mut active_count = 0;
    let mut upstream_accepted = 0;
    let mut upstream_rejected = 0;
    let mut upstream_accepted_work = TotalWork::ZERO;
    let mut upstream_rejected_work = TotalWork::ZERO;
    let metatron = router.metatron();

    for order in router.orders().iter() {
        let order_upstream_accepted = order.upstream.accepted();
        let order_upstream_rejected = order.upstream.rejected();
        let order_upstream_accepted_work = order.upstream.accepted_work();
        let order_upstream_rejected_work = order.upstream.rejected_work();
        let upstream_id = order.upstream.id();
        let status = order.status();

        orders.push(OrderStatusResponse {
            id: order.id,
            status,
            target_work: order.target_work,
            upstream_id,
            endpoint: order.upstream.endpoint().to_string(),
            username: order.upstream.username().to_string(),
            ping_ms: order.upstream.ping_ms(),
            upstream_accepted: order_upstream_accepted,
            upstream_rejected: order_upstream_rejected,
            upstream_accepted_work: order_upstream_accepted_work,
            upstream_rejected_work: order_upstream_rejected_work,
            upstream_hash_days: (order_upstream_accepted_work + order_upstream_rejected_work)
                .to_hash_days(),
            session_count: router.upstream_session_count(upstream_id),
            disconnected_count: router.upstream_disconnected_count(upstream_id),
            idle_count: router.upstream_idle_count(upstream_id),
            stats: MiningStats::from_snapshot(&router.upstream_snapshot(upstream_id), now),
        });

        if order.is_active() {
            active_count += 1;
            upstream_accepted += order_upstream_accepted;
            upstream_rejected += order_upstream_rejected;
            upstream_accepted_work += order_upstream_accepted_work;
            upstream_rejected_work += order_upstream_rejected_work;
        }
    }

    Json(RouterStatus {
        upstream_count: active_count,
        session_count: metatron.total_sessions(),
        disconnected_count: metatron.total_disconnected(),
        idle_count: metatron.total_idle(),
        uptime_secs: metatron.uptime().as_secs(),
        orders,
        upstream_accepted,
        upstream_rejected,
        upstream_accepted_work,
        upstream_rejected_work,
        upstream_hash_days: (upstream_accepted_work + upstream_rejected_work).to_hash_days(),
        stats: MiningStats::from_snapshot(&metatron.snapshot(), now),
    })
}

async fn order_detail(
    State(router): State<Arc<Router>>,
    Path(id): Path<u32>,
) -> ServerResult<Response> {
    let order = router
        .get_order(id)
        .ok_or_not_found(|| format!("Order {id}"))?;

    let now = Instant::now();
    let metatron = router.metatron();
    let upstream_id = order.upstream.id();
    let sessions = router.upstream_sessions(upstream_id);
    let session_details = sessions
        .iter()
        .map(|session| SessionDetail::from_session(session, now))
        .collect();
    let workers = metatron
        .users()
        .iter()
        .flat_map(|user| user.workers().collect::<Vec<_>>())
        .filter_map(|worker| {
            let session_count = worker.upstream_session_count(upstream_id);
            (session_count > 0).then(|| WorkerDetail {
                name: worker.workername().to_string(),
                session_count,
                stats: MiningStats::from_snapshot(&worker.upstream_snapshot(upstream_id), now),
            })
        })
        .collect();

    Ok(Json(OrderDetail {
        id: order.id,
        status: order.status(),
        target: order.target.clone(),
        target_work: order.target_work,
        upstream_id,
        upstream: UpstreamInfo::from_upstream(&order.upstream),
        user_count: router.upstream_user_count(upstream_id),
        worker_count: router.upstream_worker_count(upstream_id),
        session_count: router.upstream_session_count(upstream_id),
        disconnected_count: router.upstream_disconnected_count(upstream_id),
        idle_count: router.upstream_idle_count(upstream_id),
        uptime_secs: metatron.uptime().as_secs(),
        workers,
        sessions: session_details,
        stats: MiningStats::from_snapshot(&router.upstream_snapshot(upstream_id), now),
    })
    .into_response())
}

async fn add_order(
    State(router): State<Arc<Router>>,
    Json(request): Json<OrderRequest>,
) -> ServerResult<Response> {
    let id = router.add_order(request).await?;

    Ok(Json(json!({ "id": id })).into_response())
}

async fn remove_order(
    State(router): State<Arc<Router>>,
    Path(id): Path<u32>,
) -> ServerResult<Response> {
    router
        .cancel_order(id)
        .ok_or_not_found(|| format!("Order {id}"))?;

    Ok(Json(json!({ "id": id })).into_response())
}
