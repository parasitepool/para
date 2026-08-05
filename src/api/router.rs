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
        .route("/api/router/premium", put(set_premium))
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
    let metatron = router.metatron();

    let txids_for = |derivation_index: u32| {
        router
            .wallet()
            .map(|wallet| wallet.txids_by_derivation_index(derivation_index))
            .unwrap_or_default()
    };

    if let Some(order) = router.get_order(id) {
        let txids = order
            .bucket
            .as_ref()
            .map(|bucket| txids_for(bucket.payment.derivation_index))
            .unwrap_or_default();

        return Ok(Json(OrderDetail::from_order(
            &order,
            &metatron,
            Instant::now(),
            txids,
        ))
        .into_response());
    }

    let entry = router
        .cold_order(id)
        .ok_or_not_found(|| format!("Order {id}"))?;

    let txids = entry
        .bucket
        .as_ref()
        .map(|bucket| txids_for(bucket.derivation_index))
        .unwrap_or_default();

    Ok(Json(OrderDetail::from_entry(id, &entry, txids)?).into_response())
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
        let payment_address = order
            .bucket
            .as_ref()
            .map(|bucket| bucket.payment.address.as_unchecked());

        if let Some(address) = &self.address
            && !matches_address(&order.upstream_target, payment_address, address)
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
            && !matches_search(order.id, &order.upstream_target, payment_address, search)
        {
            return false;
        }

        true
    }

    fn matches_entry(&self, id: u32, entry: &entry::OrderEntry) -> bool {
        let payment_address = entry.bucket.as_ref().map(|bucket| &bucket.address);

        if let Some(address) = &self.address
            && !matches_address(&entry.upstream_target, payment_address, address)
        {
            return false;
        }

        if !self.statuses.is_empty() && !self.statuses.contains(&entry.status) {
            return false;
        }

        if let Some(review) = self.review
            && entry.review != review
        {
            return false;
        }

        if let Some(search) = &self.search
            && !matches_search(id, &entry.upstream_target, payment_address, search)
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

fn matches_address(
    upstream_target: &UpstreamTarget,
    payment_address: Option<&Address<NetworkUnchecked>>,
    address: &Address<NetworkUnchecked>,
) -> bool {
    upstream_target.username().address() == address || payment_address == Some(address)
}

fn matches_search(
    id: u32,
    upstream_target: &UpstreamTarget,
    payment_address: Option<&Address<NetworkUnchecked>>,
    search: &str,
) -> bool {
    id.to_string().contains(search)
        || upstream_target.to_string().to_lowercase().contains(search)
        || payment_address.is_some_and(|address| {
            address
                .clone()
                .assume_checked()
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

    let (live, cold) = router.order_snapshots(|id, entry| query.matches_entry(id, entry));

    Ok(Json(merge_summaries(&live, cold, &query, now)).into_response())
}

fn merge_summaries(
    live: &[Arc<Order>],
    cold: Vec<(u32, entry::OrderEntry)>,
    query: &OrdersQuery,
    now: Instant,
) -> Vec<OrderSummary> {
    let mut orders = live
        .iter()
        .filter(|order| query.matches(order))
        .map(|order| OrderSummary::from_order(order, now))
        .collect::<Vec<_>>();

    for (id, entry) in cold {
        match OrderSummary::from_entry(id, &entry, now) {
            Ok(summary) => orders.push(summary),
            Err(err) => warn!("Skipping cold order {id} with invalid entry: {err:#}"),
        }
    }

    orders.sort_by_key(|order| Reverse(order.id));
    orders.truncate(query.limit.unwrap_or(usize::MAX));

    orders
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

#[derive(Deserialize)]
struct PremiumRequest {
    premium_percent: f64,
}

#[derive(Serialize)]
struct PremiumResponse {
    premium_percent: f64,
}

async fn set_premium(
    _: AdminAuth,
    State(router): State<Arc<Router>>,
    Json(request): Json<PremiumRequest>,
) -> ServerResult<Response> {
    if !request.premium_percent.is_finite() || request.premium_percent <= -100.0 {
        return Err(ServerError::BadRequest(
            "premium_percent must be finite and greater than -100".into(),
        ));
    }

    router.set_premium_percent(request.premium_percent);
    Ok(Json(PremiumResponse {
        premium_percent: router.premium_percent(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(status: OrderStatus) -> entry::OrderEntry {
        entry::OrderEntry {
            status,
            review: Review::Clean,
            upstream_target: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            bucket: Some(entry::BucketEntry {
                target: HashDays::new(100.0).unwrap(),
                address: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
                    .parse()
                    .unwrap(),
                derivation_index: 3,
                amount_sat: 1_000,
                created_at_height: 42,
            }),
            created_at_secs: 1_700_000_000.0,
            stats: Stats::new().to_entry(Instant::now()),
        }
    }

    #[test]
    fn matches_entry_filters() {
        #[track_caller]
        fn case(raw: Option<&str>, expected: bool) {
            let query = OrdersQuery::parse(raw).ok().unwrap();
            assert_eq!(
                query.matches_entry(7, &test_entry(OrderStatus::Expired)),
                expected,
                "query: {raw:?}",
            );
        }

        case(None, true);
        case(Some("status=expired"), true);
        case(Some("status=active"), false);
        case(Some("status=active,expired"), true);
        case(Some("review=clean"), true);
        case(Some("review=flagged"), false);
        case(Some("search=bar"), true);
        case(Some("search=baz"), false);
        case(Some("search=7"), true);
        case(Some("search=krrl"), true);
        case(
            Some("address=tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"),
            true,
        );
        case(
            Some("address=tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"),
            false,
        );
    }

    #[test]
    fn order_detail_from_entry_maps_fields() {
        let entry = test_entry(OrderStatus::Expired);
        let detail = OrderDetail::from_entry(7, &entry, Vec::new()).unwrap();

        assert_eq!(detail.id, 7);
        assert_eq!(detail.status, OrderStatus::Expired);
        assert_eq!(detail.created_at, 1_700_000_000);
        assert_eq!(detail.created_at_height, Some(42));
        assert_eq!(detail.payment_amount, Some(Amount::from_sat(1_000)));
        assert_eq!(
            detail.payment_address.unwrap().assume_checked().to_string(),
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
        );
        assert_eq!(detail.upstream.accepted_shares, 0);
        assert!(detail.sessions.is_empty());
        assert_eq!(detail.downstream.accepted_shares, 0);
    }

    #[test]
    fn order_summary_from_entry_maps_fields() {
        let entry = test_entry(OrderStatus::Fulfilled);
        let summary = OrderSummary::from_entry(7, &entry, Instant::now()).unwrap();

        assert_eq!(summary.id, 7);
        assert_eq!(summary.status, OrderStatus::Fulfilled);
        assert_eq!(summary.endpoint, "bar:3333");
        assert_eq!(summary.requested_hash_days.unwrap().as_f64(), 100.0);
    }

    #[test]
    fn merge_summaries_sorts_descending_and_truncates_across_tiers() {
        let (metatron, _directory) = Metatron::test();
        let metatron = Arc::new(metatron);

        fn live_order(id: u32, metatron: &Arc<Metatron>) -> Arc<Order> {
            let order = Order::new(
                id,
                "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                    .parse()
                    .unwrap(),
                None,
                CancellationToken::new(),
                metatron.clone(),
            );
            order.force_status(OrderStatus::Active);
            order
        }

        let live = vec![live_order(5, &metatron), live_order(3, &metatron)];
        let cold = vec![
            (4, test_entry(OrderStatus::Expired)),
            (2, test_entry(OrderStatus::Fulfilled)),
        ];

        let now = Instant::now();

        let merged = merge_summaries(&live, cold.clone(), &OrdersQuery::default(), now);
        assert_eq!(
            merged.iter().map(|summary| summary.id).collect::<Vec<_>>(),
            [5, 4, 3, 2],
        );

        let query = OrdersQuery {
            limit: Some(3),
            ..OrdersQuery::default()
        };

        let truncated = merge_summaries(&live, cold, &query, now);
        assert_eq!(
            truncated
                .iter()
                .map(|summary| summary.id)
                .collect::<Vec<_>>(),
            [5, 4, 3],
        );
    }
}
