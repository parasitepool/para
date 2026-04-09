use {super::*, axum::extract::Query};

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
        .route("/api/router/orders", get(list_orders))
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
    let metatron = router.metatron();
    let mut orders = Vec::new();
    let mut upstream_accepted = 0;
    let mut upstream_rejected = 0;
    let mut upstream_accepted_work = TotalWork::ZERO;
    let mut upstream_rejected_work = TotalWork::ZERO;

    for order in router.orders().iter() {
        let detail = OrderDetail::from_order(order, &metatron, now);
        if let Some(ref upstream) = detail.upstream {
            upstream_accepted += upstream.accepted;
            upstream_rejected += upstream.rejected;
            upstream_accepted_work += upstream.accepted_work;
            upstream_rejected_work += upstream.rejected_work;
        }
        orders.push(detail);
    }

    Json(RouterStatus {
        order_count: orders.len(),
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

    let metatron = router.metatron();

    Ok(Json(OrderDetail::from_order(&order, &metatron, Instant::now())).into_response())
}

async fn add_order(
    State(router): State<Arc<Router>>,
    Json(request): Json<OrderRequest>,
) -> Response {
    let order = router.add_order(request, false);

    Json(json!({
        "id": order.id,
        "address": order.payment.address.to_string(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct OrdersQuery {
    address: Option<String>,
}

async fn list_orders(
    State(router): State<Arc<Router>>,
    Query(query): Query<OrdersQuery>,
) -> Json<Vec<u32>> {
    Json(
        router
            .orders()
            .iter()
            .filter(|order| {
                query.address.as_deref().is_none_or(|addr| {
                    order
                        .target
                        .username()
                        .address_str()
                        .is_some_and(|address| address == addr)
                })
            })
            .map(|order| order.id)
            .collect(),
    )
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
