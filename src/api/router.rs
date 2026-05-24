use {
    super::*,
    crate::http_server::auth::{AdminAuth, ApiAuth, BearerAuth},
    axum::extract::Query,
};

pub(crate) fn router(
    state: Arc<Router>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
    logs: Arc<logs::Logs>,
    http_api_token: Option<&str>,
    http_admin_token: Option<&str>,
) -> axum::Router {
    let auth = BearerAuth::new(http_api_token, http_admin_token);

    axum::Router::new()
        .route("/", get(home))
        .route("/order/{id}", get(order_page))
        .route("/api/router/status", get(status))
        .route("/api/router/order", post(add_order))
        .route("/api/router/order/{id}", get(order_detail))
        .route("/api/router/orders", get(list_orders))
        .route("/api/router/order/{id}/cancel", post(cancel_order))
        .route("/api/router/halt", put(set_halt))
        .route("/api/router/boost", put(set_boost))
        .route("/api/router/capacity", put(set_capacity))
        .with_state(state)
        .merge(common_routes())
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
        .layer(Extension(auth))
}

async fn home(Extension(chain): Extension<Chain>) -> ServerResult<Response> {
    Ok(render_page(RouterHtml, chain))
}

async fn order_page(Extension(chain): Extension<Chain>) -> ServerResult<Response> {
    Ok(render_page(OrderHtml, chain))
}

async fn status(_: ApiAuth, State(router): State<Arc<Router>>) -> ServerResult<Response> {
    Ok(Json(router.status()).into_response())
}

async fn order_detail(
    _: ApiAuth,
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
    _: ApiAuth,
    State(router): State<Arc<Router>>,
    Json(request): Json<OrderRequest>,
) -> ServerResult<Response> {
    let order = router.add_bucket_order(
        request.upstream_target,
        request.hash_days,
        request.hash_price,
    )?;

    let Some(bucket) = &order.bucket else {
        return Err(anyhow!("bucket order missing bucket").into());
    };

    Ok((
        StatusCode::CREATED,
        [(
            axum::http::header::LOCATION,
            format!("/api/router/order/{}", order.id),
        )],
        Json(OrderResponse::from_order(&order, bucket)),
    )
        .into_response())
}

#[derive(Deserialize)]
struct OrdersQuery {
    address: Option<Address<NetworkUnchecked>>,
}

async fn list_orders(
    _: ApiAuth,
    State(router): State<Arc<Router>>,
    Query(query): Query<OrdersQuery>,
) -> ServerResult<Response> {
    let now = Instant::now();

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
            .map(|order| OrderSummary::from_order(order, now))
            .collect::<Vec<OrderSummary>>(),
    )
    .into_response())
}

#[derive(Deserialize)]
struct ToggleRequest {
    enabled: bool,
}

#[derive(Serialize)]
struct HaltResponse {
    halt: bool,
}

async fn set_halt(
    _: AdminAuth,
    State(router): State<Arc<Router>>,
    Json(request): Json<ToggleRequest>,
) -> ServerResult<Response> {
    router.set_halt(request.enabled);
    Ok(Json(HaltResponse {
        halt: router.halt(),
    })
    .into_response())
}

#[derive(Serialize)]
struct BoostResponse {
    boost: bool,
}

async fn set_boost(
    _: AdminAuth,
    State(router): State<Arc<Router>>,
    Json(request): Json<ToggleRequest>,
) -> ServerResult<Response> {
    router.set_boost(request.enabled);
    Ok(Json(BoostResponse {
        boost: router.boost(),
    })
    .into_response())
}

#[derive(Deserialize)]
struct CapacityRequest {
    capacity_work: HashDays,
}

#[derive(Serialize)]
struct CapacityResponse {
    capacity_work: HashDays,
}

async fn set_capacity(
    _: AdminAuth,
    State(router): State<Arc<Router>>,
    Json(request): Json<CapacityRequest>,
) -> ServerResult<Response> {
    router.set_capacity_work(request.capacity_work);
    Ok(Json(CapacityResponse {
        capacity_work: router.capacity_work(),
    })
    .into_response())
}

async fn cancel_order(
    _: AdminAuth,
    State(router): State<Arc<Router>>,
    Path(id): Path<u32>,
) -> ServerResult<Response> {
    router
        .cancel_order(id)
        .ok_or_not_found(|| format!("Order {id}"))?;

    Ok(StatusCode::NO_CONTENT.into_response())
}
