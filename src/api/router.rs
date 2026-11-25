use {
    super::*,
    crate::http_server::{
        auth::{AdminAuth, ApiAuth, BearerAuth, NavbarAuth},
        error::ServerError,
    },
    axum::extract::RawQuery,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletInfo {
    pub synced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterStatus {
    pub uptime_secs: u64,
    pub block_count: u64,
    pub recent_blocks: Vec<BlockHash>,
    pub hash_price: HashPrice,
    pub hash_value: HashValue,
    pub difficulty_multiplier: f64,
    pub total_capacity_hash_days: HashDays,
    pub used_capacity_hash_days: HashDays,
    pub halt: bool,
    pub pace: bool,
    pub wallet: WalletInfo,
    pub target_hashrate: HashRate,
    pub deficit_hashrate: HashRate,
    pub surplus_hashrate: HashRate,
    pub starving_order_count: usize,
    pub upstream: UpstreamStats,
    pub downstream: DownstreamStats,
    pub git_commit: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OrderRequest {
    pub upstream_target: UpstreamTarget,
    pub hash_days: HashDays,
    pub hash_price: HashPrice,
    pub hash_rate: Option<HashRate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrderResponse {
    pub order_id: u32,
    pub payment_address: Address<NetworkUnchecked>,
    pub payment_amount: Amount,
    pub hash_price: HashPrice,
}

impl OrderResponse {
    pub(crate) fn from_order(order: &Order, terms: &Terms) -> Self {
        Self {
            order_id: order.id,
            payment_address: terms.payment.address.as_unchecked().clone(),
            payment_amount: terms.payment.amount,
            hash_price: terms.hash_price(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TapRequest {
    pub upstream_target: UpstreamTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TapResponse {
    pub tap_id: u32,
    pub payment_address: Address<NetworkUnchecked>,
    pub upstream_target: UpstreamTarget,
}

impl TapResponse {
    pub(crate) fn from_tap(tap: &Tap) -> Self {
        Self {
            tap_id: tap.id,
            payment_address: tap.address.as_unchecked().clone(),
            upstream_target: tap.upstream_target.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapSummary {
    pub id: u32,
    pub status: TapStatus,
    pub endpoint: String,
    pub username: String,
    pub payment_address: Address<NetworkUnchecked>,
    pub order_count: usize,
}

impl TapSummary {
    pub(crate) fn from_tap(tap: &Tap) -> Self {
        Self {
            id: tap.id,
            status: tap.status(),
            endpoint: tap.upstream_target.endpoint().to_string(),
            username: tap.upstream_target.username().to_string(),
            payment_address: tap.address.as_unchecked().clone(),
            order_count: tap.order_count(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapDetail {
    pub id: u32,
    pub status: TapStatus,
    pub upstream_target: UpstreamTarget,
    pub payment_address: Address<NetworkUnchecked>,
    pub created_at: u64,
    pub created_at_height: u32,
    pub orders: Vec<u32>,
}

impl TapDetail {
    pub(crate) fn from_tap(tap: &Tap, now: Instant) -> Self {
        Self {
            id: tap.id,
            status: tap.status(),
            upstream_target: tap.upstream_target.clone(),
            payment_address: tap.address.as_unchecked().clone(),
            created_at: epoch::instant_to_epoch_secs(tap.created_at, now) as u64,
            created_at_height: tap.created_at_height,
            orders: tap.order_ids(),
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
    pub target_hashrate: Option<HashRate>,
    pub hashrate: HashRate,
    pub delivered_hash_days: HashDays,
    pub best_share: Option<Difficulty>,
    pub tap_id: Option<u32>,
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
            requested_hash_days: order.target(),
            target_hashrate: order.target_hashrate(),
            hashrate: stats.hashrate_1m(now),
            delivered_hash_days: stats.delivered_work().to_hash_days(),
            best_share: stats.best_share,
            tap_id: order.payment().and_then(|payment| payment.tap_id),
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
            target_hashrate: entry
                .bucket
                .as_ref()
                .map(|bucket| Bucket::from(bucket).target_hashrate()),
            hashrate: stats.hashrate_1m(now),
            delivered_hash_days: stats.delivered_work().to_hash_days(),
            best_share: stats.best_share,
            tap_id: entry.bucket.as_ref().and_then(|bucket| bucket.tap_id),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderDetail {
    pub id: u32,
    pub status: OrderStatus,
    pub review: Review,
    pub upstream_target: UpstreamTarget,
    pub upstream_address: Option<SocketAddr>,
    pub upstream_disconnects_total: usize,
    pub requested_hash_days: Option<HashDays>,
    pub target_hashrate: Option<HashRate>,
    pub hash_price: Option<HashPrice>,
    pub payment_address: Option<Address<NetworkUnchecked>>,
    pub payment_amount: Option<Amount>,
    pub tap_id: Option<u32>,
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
        let payment = order.payment();
        let disconnects = metatron.connections().order_disconnects(order.id);

        let (sessions, downstream) = match &upstream_conn {
            Some(upstream) => metatron.downstream_snapshot(upstream.id(), now),
            None => (Vec::new(), Stats::new()),
        };

        Self {
            id: order.id,
            status: order.status(),
            review: order.review(),
            upstream_target: order.upstream_target.clone(),
            upstream_address: upstream_conn
                .as_ref()
                .and_then(|upstream| upstream.upstream_address()),
            upstream_disconnects_total: disconnects,
            requested_hash_days: order.target(),
            target_hashrate: order.target_hashrate(),
            hash_price: order.hash_price(),
            payment_address: payment.map(|payment| payment.address.as_unchecked().clone()),
            payment_amount: payment.map(|payment| payment.amount),
            tap_id: payment.and_then(|payment| payment.tap_id),
            txids,
            created_at: epoch::instant_to_epoch_secs(order.created_at, now) as u64,
            created_at_height: payment.map(|payment| payment.created_at_height),
            upstream: MiningStats::from_stats(&order.stats(), now),
            downstream: MiningStats::from_stats(&downstream, now),
            sessions: sessions
                .into_iter()
                .map(|session| SessionDetail::from_session(session.as_ref(), now))
                .collect(),
        }
    }

    pub(crate) fn from_entry(
        id: u32,
        entry: &entry::OrderEntry,
        connections: &Connections,
        txids: Vec<Txid>,
    ) -> Result<Self> {
        let now = Instant::now();
        let disconnects = connections.order_disconnects(id);
        let bucket = entry.bucket.as_ref();
        let stats = Stats::from_entry(entry.stats.clone())?;

        Ok(Self {
            id,
            status: entry.status,
            review: entry.review,
            upstream_target: entry.upstream_target.clone(),
            upstream_address: None,
            upstream_disconnects_total: disconnects,
            requested_hash_days: bucket.map(|bucket| bucket.target),
            target_hashrate: bucket.map(|bucket| Bucket::from(bucket).target_hashrate()),
            hash_price: bucket.map(|bucket| {
                HashPrice::from_total(Amount::from_sat(bucket.amount_sat), bucket.target)
            }),
            payment_address: bucket.map(|bucket| bucket.address.clone()),
            payment_amount: bucket.map(|bucket| Amount::from_sat(bucket.amount_sat)),
            tap_id: bucket.and_then(|bucket| bucket.tap_id),
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
        .route("/taps", get(taps_page))
        .route("/tap/{id}", get(tap_page))
        .route("/review", get(review_page))
        .route("/api/router/status", get(status))
        .route("/api/router/order", post(add_order))
        .route("/api/router/order/{id}", get(order_detail))
        .route("/api/router/orders", get(list_orders))
        .route("/api/router/orders/count", get(count_orders))
        .route("/api/router/order/{id}/cancel", post(cancel_order))
        .route("/api/router/order/{id}/clear", post(clear_order))
        .route("/api/router/order/{id}/refund", post(refund_order))
        .route("/api/router/tap", post(add_tap))
        .route("/api/router/tap/{id}", get(tap_detail))
        .route("/api/router/taps", get(list_taps))
        .route("/api/router/taps/count", get(count_taps))
        .route("/api/router/tap/{id}/cancel", post(cancel_tap))
        .route("/api/router/halt", put(set_halt))
        .route("/api/router/pace", put(set_pace))
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

async fn taps_page(Extension(chain): Extension<Chain>, auth: NavbarAuth) -> ServerResult<Response> {
    Ok(render_page(TapsHtml, chain, auth))
}

async fn tap_page(Extension(chain): Extension<Chain>, auth: NavbarAuth) -> ServerResult<Response> {
    Ok(render_page(TapHtml, chain, auth))
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

    let txids_for = |derivation_index: u32, outpoint: Option<OutPoint>| {
        router
            .wallet()
            .map(Wallet::last_snapshot)
            .map(|snapshot| match outpoint {
                Some(outpoint) => snapshot.txids_for_outpoint(outpoint),
                None => snapshot.txids_by_derivation_index(derivation_index),
            })
            .unwrap_or_default()
    };

    if let Some(order) = router.get_order(id) {
        let txids = order
            .payment()
            .map(|payment| txids_for(payment.derivation_index, payment.outpoint))
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
        .map(|bucket| txids_for(bucket.derivation_index, bucket.outpoint))
        .unwrap_or_default();

    Ok(Json(OrderDetail::from_entry(
        id,
        &entry,
        metatron.connections(),
        txids,
    )?)
    .into_response())
}

async fn add_order(
    _: ApiAuth,
    State(router): State<Arc<Router>>,
    Json(request): Json<OrderRequest>,
) -> ServerResult<Response> {
    let order = router
        .blocking(move |router| {
            router.add_bucket_order(
                request.upstream_target,
                request.hash_days,
                request.hash_rate,
                request.hash_price,
            )
        })
        .await??;

    let terms = order.terms.as_ref().context("bucket order missing terms")?;

    Ok((
        StatusCode::CREATED,
        [(
            axum::http::header::LOCATION,
            format!("/api/router/order/{}", order.id),
        )],
        Json(OrderResponse::from_order(&order, terms)),
    )
        .into_response())
}

async fn add_tap(
    _: ApiAuth,
    State(router): State<Arc<Router>>,
    Json(request): Json<TapRequest>,
) -> ServerResult<Response> {
    let tap = router
        .blocking(move |router| router.add_tap(request.upstream_target))
        .await??;

    Ok((
        StatusCode::CREATED,
        [(
            axum::http::header::LOCATION,
            format!("/api/router/tap/{}", tap.id),
        )],
        Json(TapResponse::from_tap(&tap)),
    )
        .into_response())
}

async fn tap_detail(
    _: ApiAuth,
    State(router): State<Arc<Router>>,
    Path(id): Path<u32>,
) -> ServerResult<Response> {
    let tap = router.get_tap(id).ok_or_not_found(|| format!("Tap {id}"))?;

    Ok(Json(TapDetail::from_tap(&tap, Instant::now())).into_response())
}

#[derive(Default)]
struct ListQuery {
    search: Option<String>,
    address: Option<Address<NetworkUnchecked>>,
    cursor: Option<u32>,
    limit: Option<usize>,
}

impl ListQuery {
    fn apply(&mut self, key: &str, value: &str) -> ServerResult<bool> {
        match key {
            "search" if !value.trim().is_empty() => {
                self.search = Some(value.trim().to_lowercase());
            }
            "search" => self.search = None,
            "address" if !value.trim().is_empty() => {
                self.address = Some(value.parse().map_err(|err| {
                    ServerError::BadRequest(format!("invalid address filter `{value}`: {err}"))
                })?);
            }
            "address" => self.address = None,
            "cursor" if !value.trim().is_empty() => {
                self.cursor = Some(value.parse().map_err(|err| {
                    ServerError::BadRequest(format!("invalid cursor `{value}`: {err}"))
                })?);
            }
            "cursor" => self.cursor = None,
            "limit" if !value.trim().is_empty() => {
                self.limit = Some(parse_usize_query_param("limit", value)?);
            }
            "limit" => self.limit = None,
            _ => return Ok(false),
        }

        Ok(true)
    }

    fn matches(
        &self,
        id: u32,
        upstream_target: &UpstreamTarget,
        payment_address: Option<&Address<NetworkUnchecked>>,
    ) -> bool {
        if let Some(address) = &self.address
            && !matches_address(upstream_target, payment_address, address)
        {
            return false;
        }

        if let Some(search) = &self.search
            && !matches_search(id, upstream_target, payment_address, search)
        {
            return false;
        }

        true
    }
}

#[derive(Default)]
struct TapsQuery {
    list: ListQuery,
    statuses: Vec<TapStatus>,
}

impl TapsQuery {
    fn parse(raw: Option<&str>) -> ServerResult<Self> {
        let mut query = Self::default();

        for (key, value) in parse_query_pairs(raw)? {
            if query.list.apply(&key, &value)? {
                continue;
            }

            if key == "status" {
                for status in value.split(',').filter(|status| !status.is_empty()) {
                    query.statuses.push(parse_tap_status(status)?);
                }
            }
        }

        Ok(query)
    }

    fn matches(&self, tap: &Tap) -> bool {
        if !self.statuses.is_empty() && !self.statuses.contains(&tap.status()) {
            return false;
        }

        self.list.matches(
            tap.id,
            &tap.upstream_target,
            Some(tap.address.as_unchecked()),
        )
    }
}

fn parse_tap_status(value: &str) -> ServerResult<TapStatus> {
    match value {
        "active" => Ok(TapStatus::Active),
        "cancelled" => Ok(TapStatus::Cancelled),
        _ => Err(ServerError::BadRequest(format!(
            "invalid tap status filter `{value}`"
        ))),
    }
}

async fn list_taps(
    _: ApiAuth,
    State(router): State<Arc<Router>>,
    RawQuery(raw_query): RawQuery,
) -> ServerResult<Response> {
    let query = TapsQuery::parse(raw_query.as_deref())?;
    let limit = effective_limit(query.list.limit)?;

    let mut taps = router
        .tap_page(query.list.cursor, limit + 1, |tap| query.matches(tap))
        .into_iter()
        .map(|tap| TapSummary::from_tap(&tap))
        .collect::<Vec<_>>();

    let next_cursor = if taps.len() > limit {
        taps.truncate(limit);
        taps.last().map(|tap| tap.id.to_string())
    } else {
        None
    };

    Ok(page_response(taps, limit, next_cursor))
}

async fn count_taps(
    _: ApiAuth,
    State(router): State<Arc<Router>>,
    RawQuery(raw_query): RawQuery,
) -> ServerResult<Response> {
    let query = TapsQuery::parse(raw_query.as_deref())?;

    if query.list.cursor.is_some() {
        return Err(ServerError::BadRequest(
            "cursor is not supported for tap counts".into(),
        ));
    }

    let count = router.count_taps(|tap| query.matches(tap));

    Ok(Json(CountResponse { count }).into_response())
}

async fn cancel_tap(
    _: AdminAuth,
    State(router): State<Arc<Router>>,
    Path(id): Path<u32>,
) -> ServerResult<Response> {
    router
        .blocking(move |router| router.cancel_tap(id))
        .await??
        .ok_or_not_found(|| format!("Tap {id}"))?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Copy, Clone)]
enum OrderKind {
    Bucket,
    Sink,
}

impl OrderKind {
    fn matches(self, has_bucket: bool) -> bool {
        match self {
            Self::Bucket => has_bucket,
            Self::Sink => !has_bucket,
        }
    }
}

#[derive(Default)]
struct OrdersQuery {
    list: ListQuery,
    statuses: Vec<OrderStatus>,
    review: Option<Review>,
    kind: Option<OrderKind>,
    tap_id: Option<u32>,
}

impl OrdersQuery {
    fn parse(raw: Option<&str>) -> ServerResult<Self> {
        let mut query = Self::default();

        for (key, value) in parse_query_pairs(raw)? {
            if query.list.apply(&key, &value)? {
                continue;
            }

            match key.as_str() {
                "status" => {
                    for status in value.split(',').filter(|status| !status.is_empty()) {
                        query.statuses.push(parse_order_status(status)?);
                    }
                }
                "review" if !value.trim().is_empty() => {
                    query.review = Some(parse_review(&value)?);
                }
                "review" => query.review = None,
                "kind" if !value.trim().is_empty() => {
                    query.kind = Some(parse_order_kind(&value)?);
                }
                "kind" => query.kind = None,
                "tap_id" if !value.trim().is_empty() => {
                    query.tap_id = Some(value.parse().map_err(|err| {
                        ServerError::BadRequest(format!("invalid tap_id filter `{value}`: {err}"))
                    })?);
                }
                "tap_id" => query.tap_id = None,
                _ => {}
            }
        }

        Ok(query)
    }

    fn matches(&self, order: &Order) -> bool {
        if !self.statuses.is_empty() && !self.statuses.contains(&order.status()) {
            return false;
        }

        if let Some(review) = self.review
            && order.review() != review
        {
            return false;
        }

        if self
            .kind
            .is_some_and(|kind| !kind.matches(!order.is_sink()))
        {
            return false;
        }

        if let Some(tap_id) = self.tap_id
            && order.payment().and_then(|payment| payment.tap_id) != Some(tap_id)
        {
            return false;
        }

        let payment_address = order
            .payment()
            .map(|payment| payment.address.as_unchecked());

        self.list
            .matches(order.id, &order.upstream_target, payment_address)
    }

    fn matches_entry(&self, id: u32, entry: &entry::OrderEntry) -> bool {
        if !self.statuses.is_empty() && !self.statuses.contains(&entry.status) {
            return false;
        }

        if let Some(review) = self.review
            && entry.review != review
        {
            return false;
        }

        if self
            .kind
            .is_some_and(|kind| !kind.matches(entry.bucket.is_some()))
        {
            return false;
        }

        if let Some(tap_id) = self.tap_id
            && entry.bucket.as_ref().and_then(|bucket| bucket.tap_id) != Some(tap_id)
        {
            return false;
        }

        let payment_address = entry.bucket.as_ref().map(|bucket| &bucket.address);

        self.list
            .matches(id, &entry.upstream_target, payment_address)
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

fn parse_order_kind(value: &str) -> ServerResult<OrderKind> {
    match value {
        "bucket" => Ok(OrderKind::Bucket),
        "sink" => Ok(OrderKind::Sink),
        _ => Err(ServerError::BadRequest(format!(
            "invalid order kind filter `{value}`"
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
    let limit = effective_limit(query.list.limit)?;

    let (live, cold) = router.order_page(
        query.list.cursor,
        limit + 1,
        |order| query.matches(order),
        |id, entry| query.matches_entry(id, entry),
    );

    let mut orders = merge_summaries(&live, cold, now);
    let next_cursor = if orders.len() > limit {
        orders.truncate(limit);
        orders.last().map(|order| order.id.to_string())
    } else {
        None
    };

    Ok(page_response(orders, limit, next_cursor))
}

async fn count_orders(
    _: ApiAuth,
    State(router): State<Arc<Router>>,
    RawQuery(raw_query): RawQuery,
) -> ServerResult<Response> {
    let query = OrdersQuery::parse(raw_query.as_deref())?;

    if query.list.cursor.is_some() {
        return Err(ServerError::BadRequest(
            "cursor is not supported for order counts".into(),
        ));
    }

    let count = router.count_orders(
        |order| query.matches(order),
        |id, entry| query.matches_entry(id, entry),
    );

    Ok(Json(CountResponse { count }).into_response())
}

fn merge_summaries(
    live: &[Arc<Order>],
    cold: Vec<(u32, entry::OrderEntry)>,
    now: Instant,
) -> Vec<OrderSummary> {
    let mut orders = live
        .iter()
        .map(|order| OrderSummary::from_order(order, now))
        .collect::<Vec<_>>();

    for (id, entry) in cold {
        match OrderSummary::from_entry(id, &entry, now) {
            Ok(summary) => orders.push(summary),
            Err(err) => warn!("Skipping cold order {id} with invalid entry: {err:#}"),
        }
    }

    orders.sort_by_key(|order| Reverse(order.id));
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
struct PaceResponse {
    pace: bool,
}

async fn set_pace(
    _: AdminAuth,
    State(router): State<Arc<Router>>,
    Json(request): Json<ToggleRequest>,
) -> ServerResult<Response> {
    router.set_pace(request.enabled);
    Ok(Json(PaceResponse {
        pace: router.pace(),
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

    let refund = router
        .blocking(move |router| router.build_refund(id, fee_rate, destination))
        .await??;

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
    use {super::*, crate::api::pagination::DEFAULT_PAGE_LIMIT};

    fn test_entry(status: OrderStatus) -> entry::OrderEntry {
        entry::OrderEntry {
            status,
            review: Review::Clean,
            upstream_target: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            bucket: Some(entry::BucketEntry {
                target: HashDays::new(100.0).unwrap(),
                hash_rate: None,
                address: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
                    .parse()
                    .unwrap(),
                derivation_index: 3,
                amount_sat: 1_000,
                created_at_height: 42,
                outpoint: None,
                tap_id: None,
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
        case(Some("kind=bucket"), true);
        case(Some("kind=sink"), false);
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
        case(Some("tap_id=0"), false);

        let mut tapped = test_entry(OrderStatus::Expired);
        tapped.bucket.as_mut().unwrap().tap_id = Some(4);
        assert!(
            OrdersQuery::parse(Some("tap_id=4"))
                .ok()
                .unwrap()
                .matches_entry(7, &tapped)
        );
        assert!(
            !OrdersQuery::parse(Some("tap_id=5"))
                .ok()
                .unwrap()
                .matches_entry(7, &tapped)
        );
        assert!(
            OrdersQuery::parse(Some("tap_id=5&tap_id="))
                .ok()
                .unwrap()
                .matches_entry(7, &tapped)
        );
        assert!(OrdersQuery::parse(Some("tap_id=foo")).is_err());
    }

    #[test]
    fn order_detail_from_entry_maps_fields() {
        let mut entry = test_entry(OrderStatus::Expired);
        entry.bucket.as_mut().unwrap().hash_rate = Some(HashRate::from_hps(50.0));
        let connections = Connections::new(Instant::now());
        let detail = OrderDetail::from_entry(7, &entry, &connections, Vec::new()).unwrap();

        assert_eq!(detail.id, 7);
        assert_eq!(detail.target_hashrate, Some(HashRate::from_hps(50.0)));
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
    fn order_disconnects_survive_retirement_but_not_restart() {
        use crate::router::testkit::*;

        let router = test_router();
        let metatron = router.metatron();
        let order = test_order(0, None, OrderStatus::Active, &metatron);
        add_orders(router.as_ref(), [order.clone()]);
        metatron.connections().record_upstream_disconnect(order.id);

        assert_eq!(
            OrderDetail::from_order(&order, &metatron, Instant::now(), Vec::new())
                .upstream_disconnects_total,
            1
        );

        order.terminate(OrderStatus::Fulfilled);
        router.persist().unwrap();
        router.retire_orders();
        drop(order);

        assert!(router.get_order(0).is_none());
        let entry = router.cold_order(0).unwrap();
        assert_eq!(
            OrderDetail::from_entry(0, &entry, metatron.connections(), Vec::new())
                .unwrap()
                .upstream_disconnects_total,
            1
        );

        let metatron = Metatron::test_with_store(metatron.store().clone());
        let entry = metatron.store().read_order(0).unwrap().unwrap();
        assert_eq!(
            OrderDetail::from_entry(0, &entry, metatron.connections(), Vec::new())
                .unwrap()
                .upstream_disconnects_total,
            0
        );
    }

    #[test]
    fn order_summary_from_entry_maps_fields() {
        let entry = test_entry(OrderStatus::Fulfilled);
        let summary = OrderSummary::from_entry(7, &entry, Instant::now()).unwrap();

        assert_eq!(summary.id, 7);
        assert_eq!(summary.status, OrderStatus::Fulfilled);
        assert_eq!(summary.endpoint, "bar:3333");
        assert_eq!(summary.requested_hash_days.unwrap().as_f64(), 100.0);
        assert_eq!(summary.target_hashrate, Some(HashRate::from_hps(100.0)));
        assert_eq!(summary.tap_id, None);
    }

    async fn json_body<T: serde::de::DeserializeOwned>(response: Response) -> T {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn list_orders_pages_across_live_and_cold() {
        use crate::router::testkit::*;

        let router = test_router();
        let metatron = router.metatron();
        add_orders(
            router.as_ref(),
            [2, 4, 6].map(|id| test_order(id, None, OrderStatus::Active, &metatron)),
        );
        add_cold_orders(
            router.as_ref(),
            [1, 3, 5].map(|id| (id, test_entry(OrderStatus::Expired))),
        );

        let page = |query: &str| {
            let router = router.router.clone();
            let query = query.to_string();
            async move {
                let response = list_orders(ApiAuth, State(router), RawQuery(Some(query)))
                    .await
                    .ok()
                    .expect("list_orders handler failed");
                let next_cursor = response
                    .headers()
                    .get("x-next-cursor")
                    .map(|cursor| cursor.to_str().unwrap().to_string());
                let ids = json_body::<Vec<OrderSummary>>(response)
                    .await
                    .iter()
                    .map(|order| order.id)
                    .collect::<Vec<_>>();
                (ids, next_cursor)
            }
        };

        assert_eq!(page("limit=2").await, (vec![6, 5], Some("5".into())));
        assert_eq!(
            page("limit=2&cursor=5").await,
            (vec![4, 3], Some("3".into()))
        );
        assert_eq!(page("limit=2&cursor=3").await, (vec![2, 1], None));
        assert_eq!(page("cursor=1").await, (vec![], None));

        assert_eq!(
            page("status=expired&limit=2").await,
            (vec![5, 3], Some("3".into()))
        );
        assert_eq!(
            page("status=expired&limit=2&cursor=3").await,
            (vec![1], None)
        );
        assert_eq!(
            page("kind=sink&limit=1&cursor=6").await,
            (vec![4], Some("4".into()))
        );

        let response = list_orders(ApiAuth, State(router.router.clone()), RawQuery(None))
            .await
            .ok()
            .unwrap();
        assert_eq!(
            response.headers()["x-limit"],
            DEFAULT_PAGE_LIMIT.to_string()
        );

        let count = |query: &str| {
            let router = router.router.clone();
            let query = query.to_string();
            async move {
                let response = count_orders(ApiAuth, State(router), RawQuery(Some(query)))
                    .await
                    .ok()
                    .expect("count_orders handler failed");
                json_body::<serde_json::Value>(response).await["count"]
                    .as_u64()
                    .unwrap()
            }
        };

        assert_eq!(count("").await, 6);
        assert_eq!(count("limit=1").await, 6);
        assert_eq!(count("status=expired").await, 3);
    }

    #[test]
    fn kind_filters_live_and_cold_orders() {
        use crate::router::testkit::*;

        let router = test_router();
        let metatron = router.metatron();
        add_orders(
            router.as_ref(),
            [
                test_order(1, None, OrderStatus::Active, &metatron),
                test_order(2, Some(hash_days(1.0)), OrderStatus::Active, &metatron),
            ],
        );
        let mut cold_sink = test_entry(OrderStatus::Expired);
        cold_sink.bucket = None;
        add_cold_orders(
            router.as_ref(),
            [(3, test_entry(OrderStatus::Expired)), (4, cold_sink)],
        );

        let bucket = OrdersQuery::parse(Some("kind=bucket")).ok().unwrap();
        assert_eq!(
            router.count_orders(
                |order| bucket.matches(order),
                |id, entry| bucket.matches_entry(id, entry),
            ),
            2
        );

        let sink = OrdersQuery::parse(Some("kind=sink")).ok().unwrap();
        assert_eq!(
            router.count_orders(
                |order| sink.matches(order),
                |id, entry| sink.matches_entry(id, entry),
            ),
            2
        );
        assert!(OrdersQuery::parse(Some("kind=unknown")).is_err());
    }

    #[test]
    fn merge_summaries_sorts_descending_across_tiers() {
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

        let merged = merge_summaries(&live, cold, now);
        assert_eq!(
            merged.iter().map(|summary| summary.id).collect::<Vec<_>>(),
            [5, 4, 3, 2],
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tap_endpoints_lifecycle_and_outpoint_scoped_order_detail() {
        use crate::router::testkit::*;

        let router = test_router();
        let wallet = router.wallet().unwrap();
        wallet.mark_synced();
        router.set_hash_value(HashValue::from_sats(100));
        router.set_premium_percent(0.0);

        let response = add_tap(
            ApiAuth,
            State(router.router.clone()),
            Json(TapRequest {
                upstream_target: test_upstream_target(),
            }),
        )
        .await
        .ok()
        .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers()["location"], "/api/router/tap/0");
        let first = json_body::<TapResponse>(response).await;
        assert_eq!(first.tap_id, 0);

        let second_target: UpstreamTarget =
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx.baz@qux:3333"
                .parse()
                .unwrap();
        let response = add_tap(
            ApiAuth,
            State(router.router.clone()),
            Json(TapRequest {
                upstream_target: second_target,
            }),
        )
        .await
        .ok()
        .unwrap();
        let second = json_body::<TapResponse>(response).await;
        assert_eq!(second.tap_id, 1);

        let first_address = first.payment_address.clone().assume_checked();
        let tx1 =
            wallet.test_receive_unconfirmed_with_salt(&first_address, Amount::from_sat(5_000), 1);
        let tx2 =
            wallet.test_receive_unconfirmed_with_salt(&first_address, Amount::from_sat(7_000), 2);
        wallet.test_confirm_tx(tx1.clone());
        wallet.test_confirm_tx(tx2.clone());
        wallet.mark_synced();
        router.process_taps();

        let detail = json_body::<TapDetail>(
            tap_detail(ApiAuth, State(router.router.clone()), Path(0))
                .await
                .ok()
                .unwrap(),
        )
        .await;
        assert_eq!(detail.id, 0);
        assert_eq!(detail.status, TapStatus::Active);
        assert_eq!(detail.orders, vec![0, 1]);

        let order0 = json_body::<OrderDetail>(
            order_detail(ApiAuth, State(router.router.clone()), Path(0))
                .await
                .ok()
                .unwrap(),
        )
        .await;
        assert_eq!(order0.tap_id, Some(0));
        assert_eq!(order0.txids, vec![tx1.compute_txid()]);

        let order1 = json_body::<OrderDetail>(
            order_detail(ApiAuth, State(router.router.clone()), Path(1))
                .await
                .ok()
                .unwrap(),
        )
        .await;
        assert_eq!(order1.tap_id, Some(0));
        assert_eq!(order1.txids, vec![tx2.compute_txid()]);

        router
            .get_order(0)
            .unwrap()
            .terminate(OrderStatus::Fulfilled);
        router.persist().unwrap();
        router.retire_orders();
        assert!(router.get_order(0).is_none());

        let cold_order0 = json_body::<OrderDetail>(
            order_detail(ApiAuth, State(router.router.clone()), Path(0))
                .await
                .ok()
                .unwrap(),
        )
        .await;
        assert_eq!(cold_order0.tap_id, Some(0));
        assert_eq!(cold_order0.txids, vec![tx1.compute_txid()]);

        let tap_orders = json_body::<Vec<OrderSummary>>(
            list_orders(
                ApiAuth,
                State(router.router.clone()),
                RawQuery(Some("tap_id=0".into())),
            )
            .await
            .ok()
            .unwrap(),
        )
        .await;
        assert_eq!(
            tap_orders
                .iter()
                .map(|order| (order.id, order.tap_id))
                .collect::<Vec<_>>(),
            vec![(1, Some(0)), (0, Some(0))],
        );

        let other_tap_orders = json_body::<Vec<OrderSummary>>(
            list_orders(
                ApiAuth,
                State(router.router.clone()),
                RawQuery(Some("tap_id=1".into())),
            )
            .await
            .ok()
            .unwrap(),
        )
        .await;
        assert!(other_tap_orders.is_empty());

        let response = cancel_tap(AdminAuth, State(router.router.clone()), Path(0))
            .await
            .ok()
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = list_taps(
            ApiAuth,
            State(router.router.clone()),
            RawQuery(Some("limit=1".into())),
        )
        .await
        .ok()
        .unwrap();
        assert_eq!(response.headers()["x-next-cursor"], "1");
        let page1 = json_body::<Vec<TapSummary>>(response).await;
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].id, 1);

        let response = list_taps(
            ApiAuth,
            State(router.router.clone()),
            RawQuery(Some("status=cancelled".into())),
        )
        .await
        .ok()
        .unwrap();
        let cancelled = json_body::<Vec<TapSummary>>(response).await;
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].id, 0);
        assert_eq!(cancelled[0].order_count, 2);

        let response = count_taps(
            ApiAuth,
            State(router.router.clone()),
            RawQuery(Some(format!("address={first_address}"))),
        )
        .await
        .ok()
        .unwrap();
        assert_eq!(
            json_body::<serde_json::Value>(response).await["count"].as_u64(),
            Some(1),
        );

        assert!(
            tap_detail(ApiAuth, State(router.router.clone()), Path(99))
                .await
                .is_err()
        );
        assert!(
            cancel_tap(AdminAuth, State(router.router.clone()), Path(99))
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn taps_query_validation_and_search() {
        use crate::router::testkit::*;

        let router = test_router();
        router.wallet().unwrap().mark_synced();
        let tap = router.add_tap(test_upstream_target()).unwrap();

        #[track_caller]
        fn case(tap: &Tap, raw: Option<&str>, expected: bool) {
            let query = TapsQuery::parse(raw).ok().unwrap();
            assert_eq!(query.matches(tap), expected, "query: {raw:?}");
        }

        case(&tap, None, true);
        case(&tap, Some("status=active"), true);
        case(&tap, Some("status=cancelled"), false);
        case(&tap, Some("status=active,cancelled"), true);
        case(&tap, Some("search=bar"), true);
        case(&tap, Some("search=baz"), false);
        case(&tap, Some("search=0"), true);
        case(
            &tap,
            Some(&format!(
                "search={}",
                tap.address.to_string().to_uppercase()
            )),
            true,
        );
        case(&tap, Some(&format!("address={}", tap.address)), true);
        case(
            &tap,
            Some("address=tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"),
            false,
        );

        for invalid in [
            "status=unknown",
            "cursor=foo",
            "address=not-an-address",
            "limit=foo",
        ] {
            assert!(
                TapsQuery::parse(Some(invalid)).is_err(),
                "expected error for {invalid}",
            );
        }

        assert!(
            list_taps(
                ApiAuth,
                State(router.router.clone()),
                RawQuery(Some("limit=0".into())),
            )
            .await
            .is_err()
        );
        assert!(
            count_taps(
                ApiAuth,
                State(router.router.clone()),
                RawQuery(Some("cursor=0".into())),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tap_routes_enforce_api_and_admin_auth() {
        use crate::router::testkit::*;

        let router = test_router();
        router.wallet().unwrap().mark_synced();

        let bitcoin_client = Arc::new(
            BitcoindClient::new(
                "http://127.0.0.1:1".into(),
                Auth::UserPass("foo".into(), "bar".into()),
                None,
                None,
                None,
            )
            .unwrap(),
        );

        let app = super::router(
            router.router.clone(),
            bitcoin_client,
            Chain::Regtest,
            logs::Logs::test(),
            Some("api-token"),
            Some("admin-token"),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();
        let base = format!("http://{addr}");
        let body = TapRequest {
            upstream_target: test_upstream_target(),
        };

        let unauth = client
            .post(format!("{base}/api/router/tap"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);

        let created = client
            .post(format!("{base}/api/router/tap"))
            .bearer_auth("api-token")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);

        let cancel_with_api = client
            .post(format!("{base}/api/router/tap/0/cancel"))
            .bearer_auth("api-token")
            .send()
            .await
            .unwrap();
        assert_eq!(cancel_with_api.status(), StatusCode::UNAUTHORIZED);

        let cancel_with_admin = client
            .post(format!("{base}/api/router/tap/0/cancel"))
            .bearer_auth("admin-token")
            .send()
            .await
            .unwrap();
        assert_eq!(cancel_with_admin.status(), StatusCode::NO_CONTENT);

        server.abort();
    }
}
