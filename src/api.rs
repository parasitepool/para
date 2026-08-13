use {
    super::*,
    axum::extract::{Path, State},
    http_server::{
        self, common_routes,
        error::{OptionExt, ServerError, ServerResult},
        templates::{
            OrderHtml, PoolHtml, ProxyHtml, ReviewHtml, RouterHtml, UserHtml, UsersHtml,
            render_page,
        },
    },
};

pub use {
    http_server::{BitcoinStatus, SystemStatus},
    pool::PoolStatus,
    proxy::ProxyStatus,
    router::{
        IntentClaimCounts, OrderDetail, OrderRequest, OrderResponse, OrderSummary, OrphanReceipt,
        PlacementCounts, RouterStatus, RoutingInfo, WalletInfo,
    },
    users::{SessionDetail, UserDetail, UserSummary, WorkerDetail},
};

pub mod pool;
pub mod proxy;
pub mod router;
pub mod users;

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
    pub accepted_shares: u64,
    pub rejected_shares: u64,
    pub accepted_work: HashWork,
    pub rejected_work: HashWork,
    pub delivered_hash_days: HashDays,
    pub best_share: Option<Difficulty>,
    pub last_share: Option<u64>,
}

impl MiningStats {
    pub(crate) fn from_stats(stats: &Stats, now: Instant) -> Self {
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
            accepted_shares: stats.accepted_shares,
            rejected_shares: stats.rejected_shares,
            accepted_work: stats.accepted_work,
            rejected_work: stats.rejected_work,
            delivered_hash_days: stats.delivered_work().to_hash_days(),
            best_share: stats.best_share,
            last_share: stats.last_share_epoch_secs(now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownstreamTotals {
    pub users: usize,
    pub workers: usize,
    pub accepted_shares: u64,
    pub rejected_shares: u64,
    pub accepted_work: HashWork,
    pub rejected_work: HashWork,
    pub delivered_hash_days: HashDays,
    pub best_share: Option<Difficulty>,
    pub last_share: Option<u64>,
}

impl DownstreamTotals {
    pub(crate) fn from_stats(users: usize, workers: usize, stats: &Stats, now: Instant) -> Self {
        Self {
            users,
            workers,
            accepted_shares: stats.accepted_shares,
            rejected_shares: stats.rejected_shares,
            accepted_work: stats.accepted_work,
            rejected_work: stats.rejected_work,
            delivered_hash_days: stats.delivered_work().to_hash_days(),
            best_share: stats.best_share,
            last_share: stats.last_share_epoch_secs(now),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamTotals {
    pub users: usize,
    pub orders: usize,
    pub accepted_shares: u64,
    pub rejected_shares: u64,
    pub accepted_work: HashWork,
    pub rejected_work: HashWork,
    pub delivered_hash_days: HashDays,
    pub best_share: Option<Difficulty>,
    pub last_share: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownstreamStats {
    pub users: usize,
    pub workers: usize,
    pub sessions: usize,
    pub idle: usize,
    pub disconnected: usize,
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
    pub accepted_shares: u64,
    pub rejected_shares: u64,
    pub accepted_work: HashWork,
    pub rejected_work: HashWork,
    pub totals: DownstreamTotals,
}

impl DownstreamStats {
    pub(crate) fn from_metatron(metatron: &Metatron, now: Instant) -> Self {
        let downstream = metatron.downstream(now);
        let traffic = &downstream.traffic;

        Self {
            users: downstream.users,
            workers: downstream.workers,
            sessions: downstream.sessions,
            idle: downstream.idle,
            disconnected: metatron.total_disconnected(),
            hashrate_1m: traffic.hashrate_1m(now),
            hashrate_5m: traffic.hashrate_5m(now),
            hashrate_15m: traffic.hashrate_15m(now),
            hashrate_1hr: traffic.hashrate_1hr(now),
            hashrate_6hr: traffic.hashrate_6hr(now),
            hashrate_1d: traffic.hashrate_1d(now),
            hashrate_7d: traffic.hashrate_7d(now),
            sps_1m: traffic.sps_1m(now),
            sps_5m: traffic.sps_5m(now),
            sps_15m: traffic.sps_15m(now),
            sps_1hr: traffic.sps_1hr(now),
            accepted_shares: traffic.accepted_shares,
            rejected_shares: traffic.rejected_shares,
            accepted_work: traffic.accepted_work,
            rejected_work: traffic.rejected_work,
            totals: DownstreamTotals::from_stats(
                metatron.total_users(),
                downstream.total_workers,
                &downstream.total,
                now,
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamStats {
    pub users: usize,
    pub workers: usize,
    pub orders: usize,
    pub pending: usize,
    pub disconnected: usize,
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
    pub accepted_shares: u64,
    pub rejected_shares: u64,
    pub accepted_work: HashWork,
    pub rejected_work: HashWork,
    pub totals: UpstreamTotals,
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
}

impl UpstreamInfo {
    pub(crate) fn from_upstream(upstream: &Upstream) -> Self {
        Self {
            endpoint: upstream.endpoint().to_string(),
            connected: upstream.is_connected(),
            ping_ms: upstream.ping_ms(),
            difficulty: upstream.difficulty(),
            username: upstream.username().clone(),
            enonce1: upstream.enonce1().clone(),
            enonce2_size: upstream.enonce2_size(),
            version_mask: upstream.version_mask(),
        }
    }
}
