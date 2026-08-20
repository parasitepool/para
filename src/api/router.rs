use {
    super::*,
    crate::http_server::{
        auth::{AdminAuth, ApiAuth, BearerAuth, NavbarAuth},
        error::ServerError,
    },
    axum::extract::RawQuery,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlacementCounts {
    pub targeted: usize,
    pub estimated: usize,
    pub blind: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OrphanReceipt {
    pub derivation_index: u32,
    pub address: Address<NetworkUnchecked>,
    pub amount: Amount,
    pub first_seen_height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingInfo {
    pub sessions_trimmed_1h: usize,
    pub intents_created_1h: usize,
    pub intents_expired_1h: usize,
    pub intent_claimed_1h: usize,
    pub placements_1h: PlacementCounts,
    pub deficit_hashrate: HashRate,
    pub bucket_order_count: usize,
    pub sink_order_count: usize,
    pub starving_order_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletInfo {
    pub synced: bool,
    pub orphan_receipts: Vec<OrphanReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterStatus {
    pub uptime_secs: u64,
    pub block_count: u64,
    pub recent_blocks: Vec<BlockHash>,
    pub hash_price: HashPrice,
    pub premium_percent: f64,
    pub total_capacity_hash_days: HashDays,
    pub used_capacity_hash_days: HashDays,
    pub halt: bool,
    pub boost: bool,
    pub wallet: WalletInfo,
    pub routing: RoutingInfo,
    pub upstream: UpstreamStats,
    pub downstream: DownstreamStats,
    pub git_commit: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OrderRequest {
    pub upstream_target: UpstreamTarget,
    pub hash_days: HashDays,
    pub hash_price: HashPrice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrderResponse {
    pub order_id: u32,
    pub payment_address: Address<NetworkUnchecked>,
    pub payment_amount: Amount,
    pub hash_price: HashPrice,
}

impl OrderResponse {
    pub(crate) fn from_order(order: &Order, bucket: &Bucket) -> Self {
        Self {
            order_id: order.id,
            payment_address: bucket.payment.address.as_unchecked().clone(),
            payment_amount: bucket.payment.amount,
            hash_price: HashPrice::from_total(bucket.payment.amount, bucket.target),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderSummary {
    pub id: u32,
    pub status: OrderStatus,
    pub review: Review,
    pub endpoint: String,
    pub username: String,
    pub requested_hash_days: Option<HashDays>,
    pub hashrate: HashRate,
    pub delivered_hash_days: HashDays,
    pub best_share: Option<Difficulty>,
}

impl OrderSummary {
    pub(crate) fn from_order(order: &Order, now: Instant) -> Self {
        let stats = order.stats();
        Self {
            id: order.id,
            status: order.status(),
            review: order.review(),
            endpoint: order.upstream_target.endpoint().to_string(),
            username: order.upstream_target.username().to_string(),
            requested_hash_days: order.bucket.as_ref().map(|bucket| bucket.target),
            hashrate: stats.hashrate_1m(now),
            delivered_hash_days: stats.delivered_work().to_hash_days(),
            best_share: stats.best_share,
        }
    }

    pub(crate) fn from_entry(id: u32, entry: &entry::OrderEntry, now: Instant) -> Result<Self> {
        let stats = Stats::from_entry(entry.stats.clone())?;
        Ok(Self {
            id,
            status: entry.status,
            review: entry.review,
            endpoint: entry.upstream_target.endpoint().to_string(),
            username: entry.upstream_target.username().to_string(),
            requested_hash_days: entry.bucket.as_ref().map(|bucket| bucket.target),
            hashrate: stats.hashrate_1m(now),
            delivered_hash_days: stats.delivered_work().to_hash_days(),
            best_share: stats.best_share,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderDetail {
    pub id: u32,
    pub status: OrderStatus,
    pub review: Review,
    pub upstream_target: UpstreamTarget,
    pub requested_hash_days: Option<HashDays>,
    pub hash_price: Option<HashPrice>,
    pub payment_address: Option<Address<NetworkUnchecked>>,
    pub payment_amount: Option<Amount>,
    pub txids: Vec<Txid>,
    pub created_at: u64,
    pub created_at_height: Option<u32>,
    pub upstream: MiningStats,
    pub downstream: MiningStats,
    pub sessions: Vec<SessionDetail>,
}

impl OrderDetail {
    pub(crate) fn from_order(
        order: &Order,
        metatron: &Metatron,
        now: Instant,
        txids: Vec<Txid>,
    ) -> Self {
        let upstream_conn = order.upstream();
        let bucket = order.bucket.as_ref();

        let (sessions, downstream) = match &upstream_conn {
            Some(upstream) => metatron.downstream_snapshot(upstream.id(), now),
            None => (Vec::new(), Stats::new()),
        };

        Self {
            id: order.id,
            status: order.status(),
            review: order.review(),
            upstream_target: order.upstream_target.clone(),
            requested_hash_days: bucket.map(|bucket| bucket.target),
            hash_price: bucket
                .map(|bucket| HashPrice::from_total(bucket.payment.amount, bucket.target)),
            payment_address: bucket.map(|bucket| bucket.payment.address.as_unchecked().clone()),
            payment_amount: bucket.map(|bucket| bucket.payment.amount),
            txids,
            created_at: epoch::instant_to_epoch_secs(order.created_at, now) as u64,
            created_at_height: bucket.map(|bucket| bucket.payment.created_at_height),
            upstream: MiningStats::from_stats(&order.stats(), now),
            downstream: MiningStats::from_stats(&downstream, now),
            sessions: sessions
                .into_iter()
                .map(|session| SessionDetail::from_session(session.as_ref(), now))
                .collect(),
        }
    }

    pub(crate) fn from_entry(id: u32, entry: &entry::OrderEntry, txids: Vec<Txid>) -> Result<Self> {
        let now = Instant::now();
        let bucket = entry.bucket.as_ref();
        let stats = Stats::from_entry(entry.stats.clone())?;

        Ok(Self {
            id,
            status: entry.status,
            review: entry.review,
            upstream_target: entry.upstream_target.clone(),
            requested_hash_days: bucket.map(|bucket| bucket.target),
            hash_price: bucket.map(|bucket| {
                HashPrice::from_total(Amount::from_sat(bucket.amount_sat), bucket.target)
            }),
            payment_address: bucket.map(|bucket| bucket.address.clone()),
            payment_amount: bucket.map(|bucket| Amount::from_sat(bucket.amount_sat)),
            txids,
            created_at: entry.created_at_secs as u64,
            created_at_height: bucket.map(|bucket| bucket.created_at_height),
            upstream: MiningStats::from_stats(&stats, now),
            downstream: MiningStats::from_stats(&Stats::new(), now),
            sessions: Vec::new(),
        })
    }
}

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
        .route("/review", get(review_page))
        .route("/api/router/status", get(status))
        .route("/api/router/order", post(add_order))
        .route("/api/router/order/{id}", get(order_detail))
        .route("/api/router/orders", get(list_orders))
        .route("/api/router/order/{id}/cancel", post(cancel_order))
        .route("/api/router/order/{id}/clear", post(clear_order))
        .route("/api/router/order/{id}/refund", post(refund_order))
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

async fn review_page(
    Extension(chain): Extension<Chain>,
    auth: NavbarAuth,
) -> ServerResult<Response> {
    Ok(render_page(ReviewHtml, chain, auth))
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

#[derive(Deserialize)]
struct RefundRequest {
    fee_rate: Option<u64>,
    destination: Option<Address<NetworkUnchecked>>,
}

#[derive(Serialize)]
struct RefundResponse {
    psbt: String,
    destination: Address,
    amount: Amount,
    outpoints: Vec<OutPoint>,
    fee_rate: u64,
}

async fn refund_order(
    _: AdminAuth,
    State(router): State<Arc<Router>>,
    Extension(chain): Extension<Chain>,
    Path(id): Path<u32>,
    Json(request): Json<RefundRequest>,
) -> ServerResult<Response> {
    let destination = request
        .destination
        .map(|address| {
            address
                .require_network(chain.network())
                .map_err(|err| ServerError::BadRequest(format!("invalid destination: {err}")))
        })
        .transpose()?;

    let fee_rate = request
        .fee_rate
        .map(|rate| {
            FeeRate::from_sat_per_vb(rate)
                .ok_or_else(|| ServerError::BadRequest("invalid fee rate".into()))
        })
        .transpose()?;

    let refund = router.build_refund(id, fee_rate, destination)?;

    Ok(Json(RefundResponse {
        psbt: refund.psbt.to_string(),
        destination: refund.destination,
        amount: refund.amount,
        outpoints: refund.outpoints,
        fee_rate: refund.fee_rate.to_sat_per_vb_ceil(),
    })
    .into_response())
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
