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
        .route("/api/router/order/{id}/cancel", post(cancel_order))
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
) -> ServerResult<Json<OrderDetail>> {
    let order = router
        .get_order(id)
        .ok_or_not_found(|| format!("Order {id}"))?;

    let metatron = router.metatron();

    Ok(Json(OrderDetail::from_order(
        &order,
        &metatron,
        Instant::now(),
    )))
}

async fn add_order(
    State(router): State<Arc<Router>>,
    Json(request): Json<OrderRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let order = router
        .add_order(request)
        .ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;

    Ok((
        StatusCode::CREATED,
        [(
            axum::http::header::LOCATION,
            format!("/api/router/order/{}", order.id),
        )],
        Json(AddOrderResponse::from_order(&order)),
    ))
}

#[derive(Deserialize)]
struct OrdersQuery {
    address: Option<Address<NetworkUnchecked>>,
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
                query
                    .address
                    .as_ref()
                    .is_none_or(|addr| order.target.username().address() == addr)
            })
            .map(|order| order.id)
            .collect(),
    )
}

async fn cancel_order(
    State(router): State<Arc<Router>>,
    Path(id): Path<u32>,
) -> ServerResult<StatusCode> {
    router
        .cancel_order(id)
        .ok_or_not_found(|| format!("Order {id}"))?;

    Ok(StatusCode::NO_CONTENT)
}
