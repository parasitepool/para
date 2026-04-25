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

async fn home(Extension(chain): Extension<Chain>) -> ServerResult<Response> {
    Ok(render_page(RouterHtml, chain))
}

async fn order_page(Extension(chain): Extension<Chain>) -> ServerResult<Response> {
    Ok(render_page(OrderHtml, chain))
}

async fn status(State(router): State<Arc<Router>>) -> ServerResult<Response> {
    Ok(Json(router.status()).into_response())
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
) -> ServerResult<Response> {
    let order = router
        .add_order(
            request.upstream_target,
            OrderKind::Bucket(request.hashdays),
            request.price,
        )
        .await?;

    Ok((
        StatusCode::CREATED,
        [(
            axum::http::header::LOCATION,
            format!("/api/router/order/{}", order.id),
        )],
        Json(OrderResponse::from_order(&order)),
    )
        .into_response())
}

#[derive(Deserialize)]
struct OrdersQuery {
    address: Option<Address<NetworkUnchecked>>,
}

async fn list_orders(
    State(router): State<Arc<Router>>,
    Query(query): Query<OrdersQuery>,
) -> ServerResult<Response> {
    let now = Instant::now();
    let metatron = router.metatron();

    Ok(Json(
        router
            .orders()
            .iter()
            .filter(|order| {
                query
                    .address
                    .as_ref()
                    .is_none_or(|addr| order.upstream_target.username().address() == addr)
            })
            .map(|order| OrderDetail::from_order(order, &metatron, now))
            .collect::<Vec<OrderDetail>>(),
    )
    .into_response())
}

async fn cancel_order(
    State(router): State<Arc<Router>>,
    Path(id): Path<u32>,
) -> ServerResult<Response> {
    router
        .cancel_order(id)
        .ok_or_not_found(|| format!("Order {id}"))?;

    Ok(StatusCode::NO_CONTENT.into_response())
}
