use {
    super::*,
    axum::extract::{Path, State},
    http_server::{
        self, common_routes,
        error::{OptionExt, ServerError, ServerResult},
        templates::{OrderHtml, PoolHtml, ProxyHtml, RouterHtml, UserHtml, UsersHtml, render_page},
    },
};

pub use http_server::{BitcoinStatus, SystemStatus};

pub mod pool;
pub mod proxy;
pub mod router;
pub(crate) mod users;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningStats {
    pub hashrate_1m: HashRate,
    pub hashrate_5m: HashRate,
    pub hashrate_15m: HashRate,
    pub hashrate_1hr: HashRate,
    pub hashrate_6hr: HashRate,
    pub hashrate_1d: HashRate,
    pub hashrate_7d: HashRate,
    pub sps_1m: f64,
    pub sps_5m: f64,
    pub sps_15m: f64,
    pub sps_1hr: f64,
    pub best_share: Option<Difficulty>,
    pub last_share: Option<u64>,
    pub accepted_shares: u64,
    pub rejected_shares: u64,
    pub accepted_work: HashWork,
    pub rejected_work: HashWork,
    pub hash_days: HashDays,
}

impl MiningStats {
    pub(crate) fn from_snapshot(stats: &Stats, now: Instant) -> Self {
        Self {
            hashrate_1m: stats.hashrate_1m(now),
            hashrate_5m: stats.hashrate_5m(now),
            hashrate_15m: stats.hashrate_15m(now),
            hashrate_1hr: stats.hashrate_1hr(now),
            hashrate_6hr: stats.hashrate_6hr(now),
            hashrate_1d: stats.hashrate_1d(now),
            hashrate_7d: stats.hashrate_7d(now),
            sps_1m: stats.sps_1m(now),
            sps_5m: stats.sps_5m(now),
            sps_15m: stats.sps_15m(now),
            sps_1hr: stats.sps_1hr(now),
            best_share: stats.best_share,
            last_share: stats
                .last_share
                .map(|time| epoch::instant_to_epoch_secs(time, now) as u64),
            accepted_shares: stats.accepted_shares,
            rejected_shares: stats.rejected_shares,
            accepted_work: stats.accepted_work,
            rejected_work: stats.rejected_work,
            hash_days: stats.accepted_work.to_hash_days(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStatus {
    pub block_count: u64,
    pub uptime_secs: u64,
    pub downstream: DownstreamInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownstreamInfo {
    pub user_count: usize,
    pub worker_count: usize,
    pub session_count: usize,
    pub disconnected_count: usize,
    pub idle_count: usize,
    pub stats: MiningStats,
}

impl DownstreamInfo {
    pub(crate) fn from_metatron(metatron: &Metatron, now: Instant) -> Self {
        Self {
            user_count: metatron.total_users(),
            worker_count: metatron.total_workers(),
            session_count: metatron.total_sessions(),
            disconnected_count: metatron.total_disconnected(),
            idle_count: metatron.total_idle(),
            stats: MiningStats::from_snapshot(&metatron.snapshot(), now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSummary {
    pub address: Address<NetworkUnchecked>,
    pub worker_count: usize,
    pub session_count: usize,
    pub hashrate: HashRate,
    pub received_hash_days: HashDays,
    pub best_share: Option<Difficulty>,
    pub last_share: Option<u64>,
}

impl UserSummary {
    pub(crate) fn from_user(user: &User, now: Instant) -> Self {
        let stats = user.snapshot();
        Self {
            address: user.address.as_unchecked().clone(),
            worker_count: user.worker_count(),
            session_count: user.session_count(),
            hashrate: stats.hashrate_1m(now),
            received_hash_days: stats.accepted_work.to_hash_days(),
            best_share: stats.best_share,
            last_share: stats
                .last_share
                .map(|time| epoch::instant_to_epoch_secs(time, now) as u64),
        }
    }
}

fn decode_query_component(value: &str) -> ServerResult<String> {
    let value = value.replace('+', " ");
    urlencoding::decode(&value)
        .map(|value| value.into_owned())
        .map_err(|err| ServerError::BadRequest(format!("invalid query encoding: {err}")))
}

fn parse_usize_query_param(name: &str, value: &str) -> ServerResult<usize> {
    value
        .parse()
        .map_err(|err| ServerError::BadRequest(format!("invalid {name} `{value}`: {err}")))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDetail {
    pub address: Address<NetworkUnchecked>,
    pub session_count: usize,
    pub authorized_at: u64,
    pub workers: Vec<WorkerDetail>,
    pub sessions: Vec<SessionDetail>,
    pub stats: MiningStats,
}

impl UserDetail {
    pub(crate) fn from_user(user: &User, now: Instant) -> Self {
        let mut workers = Vec::new();
        let mut sessions = Vec::new();

        for worker in user.workers() {
            sessions.extend(
                worker
                    .sessions()
                    .map(|s| SessionDetail::from_session(&s, now)),
            );
            workers.push(WorkerDetail::from_worker(&worker, now));
        }

        let user_stats = user.snapshot();

        Self {
            address: user.address.as_unchecked().clone(),
            session_count: user.session_count(),
            authorized_at: user.authorized,
            workers,
            sessions,
            stats: MiningStats::from_snapshot(&user_stats, now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerDetail {
    pub name: String,
    pub session_count: usize,
    pub stats: MiningStats,
}

impl WorkerDetail {
    pub(crate) fn from_worker(worker: &Worker, now: Instant) -> Self {
        let stats = worker.snapshot();
        Self {
            name: worker.workername().to_string(),
            session_count: worker.session_count(),
            stats: MiningStats::from_snapshot(&stats, now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDetail {
    pub id: SessionId,
    pub order_id: u32,
    pub address: Address<NetworkUnchecked>,
    pub worker_name: String,
    pub username: String,
    pub enonce1: Extranonce,
    pub version_mask: Option<Version>,
    pub stats: MiningStats,
}

impl SessionDetail {
    pub(crate) fn from_session(session: &Session, now: Instant) -> Self {
        let stats = session.snapshot();
        Self {
            id: session.id(),
            order_id: session.id().order_id(),
            address: session.address().as_unchecked().clone(),
            worker_name: session.workername().to_string(),
            username: session.username().to_string(),
            enonce1: session.enonce1().clone(),
            version_mask: session.version_mask(),
            stats: MiningStats::from_snapshot(&stats, now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyStatus {
    pub uptime_secs: u64,
    pub upstream: UpstreamInfo,
    pub downstream: DownstreamInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamInfo {
    pub endpoint: String,
    pub connected: bool,
    pub ping_ms: u128,
    pub difficulty: Difficulty,
    pub username: Username,
    pub enonce1: Extranonce,
    pub enonce2_size: usize,
    pub version_mask: Option<Version>,
    pub stats: MiningStats,
}

impl UpstreamInfo {
    pub(crate) fn from_upstream(upstream: &Upstream, metatron: &Metatron, now: Instant) -> Self {
        let stats = metatron.order_stats(upstream.id());
        Self {
            endpoint: upstream.endpoint().to_string(),
            connected: upstream.is_connected(),
            ping_ms: upstream.ping_ms(),
            difficulty: upstream.difficulty(),
            username: upstream.username().clone(),
            enonce1: upstream.enonce1().clone(),
            enonce2_size: upstream.enonce2_size(),
            version_mask: upstream.version_mask(),
            stats: MiningStats::from_snapshot(&stats, now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterStatus {
    pub uptime_secs: u64,
    pub hash_price: HashPrice,
    pub capacity_work: HashDays,
    pub available_work: HashDays,
    pub active_order_count: usize,
    pub wallet_synced: bool,
    pub halt: bool,
    pub boost: bool,
    pub upstream: MiningStats,
    pub downstream: DownstreamInfo,
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
            delivered_hash_days: stats.accepted_work.to_hash_days(),
            best_share: stats.best_share,
        }
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
    pub created_at: u64,
    pub created_at_height: Option<u32>,
    pub upstream: MiningStats,
    pub downstream: MiningStats,
    pub sessions: Vec<SessionDetail>,
}

impl OrderDetail {
    pub(crate) fn from_order(order: &Order, metatron: &Metatron, now: Instant) -> Self {
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
            created_at: epoch::instant_to_epoch_secs(order.created_at, now) as u64,
            created_at_height: bucket.map(|bucket| bucket.payment.created_at_height),
            upstream: MiningStats::from_snapshot(&order.stats(), now),
            downstream: MiningStats::from_snapshot(&downstream, now),
            sessions: sessions
                .into_iter()
                .map(|session| SessionDetail::from_session(session.as_ref(), now))
                .collect(),
        }
    }
}
