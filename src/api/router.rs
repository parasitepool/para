use {
    super::*,
    crate::http_server::{
        auth::{AdminAuth, ApiAuth, BearerAuth, NavbarAuth},
        error::ServerError,
    },
    axum::extract::RawQuery,
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
    let metatron = state.metatron();

    axum::Router::new()
        .route("/", get(home))
        .route("/order/{id}", get(order_page))
        .route("/api/router/status", get(status))
        .route("/api/router/order", post(add_order))
        .route("/api/router/order/{id}", get(order_detail))
        .route("/api/router/orders", get(list_orders))
        .route("/api/router/order/{id}/cancel", post(cancel_order))
        .route("/api/router/order/{id}/clear", post(clear_order))
        .route("/api/router/halt", put(set_halt))
        .route("/api/router/boost", put(set_boost))
        .route("/api/router/capacity", put(set_capacity))
        .with_state(state)
        .merge(users::routes(users::Service::Router, metatron))
        .merge(common_routes())
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
        .layer(Extension(auth))
}

async fn home(Extension(chain): Extension<Chain>, auth: NavbarAuth) -> ServerResult<Response> {
    Ok(render_page(RouterHtml, chain, auth))
}

async fn order_page(
    Extension(chain): Extension<Chain>,
    auth: NavbarAuth,
) -> ServerResult<Response> {
    Ok(render_page(OrderHtml, chain, auth))
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

#[derive(Default)]
struct OrdersQuery {
    search: Option<String>,
    address: Option<Address<NetworkUnchecked>>,
    statuses: Vec<OrderStatus>,
    review: Option<Review>,
    limit: Option<usize>,
}

impl OrdersQuery {
    fn parse(raw: Option<&str>) -> ServerResult<Self> {
        let mut query = Self::default();

        let Some(raw) = raw else {
            return Ok(query);
        };

        for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = decode_query_component(key)?;
            let value = decode_query_component(value)?;

            match key.as_str() {
                "search" if !value.trim().is_empty() => {
                    query.search = Some(value.trim().to_lowercase());
                }
                "search" => query.search = None,
                "address" if !value.trim().is_empty() => {
                    query.address = Some(value.parse().map_err(|err| {
                        ServerError::BadRequest(format!("invalid address filter `{value}`: {err}"))
                    })?);
                }
                "address" => query.address = None,
                "status" => {
                    for status in value.split(',').filter(|status| !status.is_empty()) {
                        query.statuses.push(parse_order_status(status)?);
                    }
                }
                "review" if !value.trim().is_empty() => {
                    query.review = Some(parse_review(&value)?);
                }
                "review" => query.review = None,
                "limit" if !value.trim().is_empty() => {
                    query.limit = Some(parse_usize_query_param("limit", &value)?);
                }
                "limit" => query.limit = None,
                _ => {}
            }
        }

        Ok(query)
    }

    fn matches(&self, order: &Order) -> bool {
        if let Some(address) = &self.address
            && !order_matches_address(order, address)
        {
            return false;
        }

        if !self.statuses.is_empty() && !self.statuses.contains(&order.status()) {
            return false;
        }

        if let Some(review) = self.review
            && order.review() != review
        {
            return false;
        }

        if let Some(search) = &self.search
            && !order_matches_search(order, search)
        {
            return false;
        }

        true
    }
}

fn parse_order_status(value: &str) -> ServerResult<OrderStatus> {
    match value {
        "pending" => Ok(OrderStatus::Pending),
        "in_mempool" => Ok(OrderStatus::InMempool),
        "active" => Ok(OrderStatus::Active),
        "fulfilled" => Ok(OrderStatus::Fulfilled),
        "cancelled" => Ok(OrderStatus::Cancelled),
        "disconnected" => Ok(OrderStatus::Disconnected),
        "expired" => Ok(OrderStatus::Expired),
        _ => Err(ServerError::BadRequest(format!(
            "invalid order status filter `{value}`"
        ))),
    }
}

fn parse_review(value: &str) -> ServerResult<Review> {
    match value {
        "clean" => Ok(Review::Clean),
        "flagged" => Ok(Review::Flagged),
        "cleared" => Ok(Review::Cleared),
        _ => Err(ServerError::BadRequest(format!(
            "invalid review filter `{value}`"
        ))),
    }
}

fn order_matches_address(order: &Order, address: &Address<NetworkUnchecked>) -> bool {
    order.upstream_target.username().address() == address
        || order
            .bucket
            .as_ref()
            .is_some_and(|bucket| bucket.payment.address.as_unchecked() == address)
}

fn order_matches_search(order: &Order, search: &str) -> bool {
    order.id.to_string().contains(search)
        || order
            .upstream_target
            .to_string()
            .to_lowercase()
            .contains(search)
        || order.bucket.as_ref().is_some_and(|bucket| {
            bucket
                .payment
                .address
                .to_string()
                .to_lowercase()
                .contains(search)
        })
}

async fn list_orders(
    _: ApiAuth,
    State(router): State<Arc<Router>>,
    RawQuery(raw_query): RawQuery,
) -> ServerResult<Response> {
    let now = Instant::now();
    let query = OrdersQuery::parse(raw_query.as_deref())?;
    let orders = router
        .orders()
        .iter()
        .rev()
        .filter(|order| query.matches(order))
        .take(query.limit.unwrap_or(usize::MAX))
        .map(|order| OrderSummary::from_order(order, now))
        .collect::<Vec<OrderSummary>>();

    Ok(Json(orders).into_response())
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
    capacity_hash_days: HashDays,
}

#[derive(Serialize)]
struct CapacityResponse {
    capacity_hash_days: HashDays,
}

async fn set_capacity(
    _: AdminAuth,
    State(router): State<Arc<Router>>,
    Json(request): Json<CapacityRequest>,
) -> ServerResult<Response> {
    router.set_capacity_work(request.capacity_hash_days);
    Ok(Json(CapacityResponse {
        capacity_hash_days: router.capacity_work(),
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

async fn clear_order(
    _: AdminAuth,
    State(router): State<Arc<Router>>,
    Path(id): Path<u32>,
) -> ServerResult<Response> {
    router
        .clear_order(id)
        .ok_or_not_found(|| format!("Order {id}"))?;

    Ok(StatusCode::NO_CONTENT.into_response())
}
