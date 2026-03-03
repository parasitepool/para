use {
    super::*,
    axum::extract::{Path, State},
    http_server::{
        self, common_routes,
        error::{OptionExt, ServerResult},
        templates::{
            PoolHtml, ProxyHtml, RouterHtml, UpstreamHtml, UserHtml, UsersHtml, render_page,
        },
    },
};

pub use http_server::{BitcoinStatus, SystemStatus};

pub mod pool;
pub mod proxy;
pub mod router;

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
    pub accepted_work: TotalWork,
    pub rejected_work: TotalWork,
    pub ph_days: PhDays,
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
                .map(|time| now.duration_since(time).as_secs()),
            accepted_shares: stats.accepted_shares,
            rejected_shares: stats.rejected_shares,
            accepted_work: stats.accepted_work,
            rejected_work: stats.rejected_work,
            ph_days: stats.accepted_work.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStatus {
    pub endpoint: String,
    pub user_count: usize,
    pub worker_count: usize,
    pub block_count: u64,
    pub session_count: usize,
    pub disconnected_count: usize,
    pub idle_count: usize,
    pub uptime_secs: u64,
    #[serde(flatten)]
    pub stats: MiningStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDetail {
    pub address: String,
    pub session_count: usize,
    pub authorized_at: u64,
    pub workers: Vec<WorkerDetail>,
    pub sessions: Vec<SessionDetail>,
    #[serde(flatten)]
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
            address: user.address.to_string(),
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
    #[serde(flatten)]
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
    pub upstream_id: u32,
    pub address: String,
    pub worker_name: String,
    pub username: String,
    pub enonce1: Extranonce,
    pub version_mask: Option<Version>,
    #[serde(flatten)]
    pub stats: MiningStats,
}

impl SessionDetail {
    pub(crate) fn from_session(session: &Session, now: Instant) -> Self {
        let stats = session.snapshot();
        Self {
            id: session.id(),
            upstream_id: session.id().upstream_id(),
            address: session.address().to_string(),
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
    pub endpoint: String,
    pub user_count: usize,
    pub worker_count: usize,
    pub session_count: usize,
    pub disconnected_count: usize,
    pub idle_count: usize,
    pub uptime_secs: u64,
    pub upstream: UpstreamInfo,
    #[serde(flatten)]
    pub stats: MiningStats,
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
    pub accepted: u64,
    pub rejected: u64,
    pub accepted_work: TotalWork,
    pub rejected_work: TotalWork,
    pub ph_days: PhDays,
}

impl UpstreamInfo {
    pub(crate) fn from_upstream(upstream: &Upstream) -> Self {
        let accepted_work = upstream.accepted_work();
        let rejected_work = upstream.rejected_work();
        Self {
            endpoint: upstream.endpoint().to_string(),
            connected: upstream.is_connected(),
            ping_ms: upstream.ping_ms(),
            difficulty: upstream.difficulty(),
            username: upstream.username().clone(),
            enonce1: upstream.enonce1().clone(),
            enonce2_size: upstream.enonce2_size(),
            version_mask: upstream.version_mask(),
            accepted: upstream.accepted(),
            rejected: upstream.rejected(),
            accepted_work,
            rejected_work,
            ph_days: (accepted_work + rejected_work).into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterStatus {
    pub upstream_count: usize,
    pub session_count: usize,
    pub uptime_secs: u64,
    pub slots: Vec<SlotStatus>,
    pub upstream_accepted: u64,
    pub upstream_rejected: u64,
    pub upstream_accepted_work: TotalWork,
    pub upstream_rejected_work: TotalWork,
    pub upstream_ph_days: PhDays,
    #[serde(flatten)]
    pub stats: MiningStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotStatus {
    pub upstream_id: u32,
    pub endpoint: String,
    pub username: String,
    pub ping_ms: u128,
    pub session_count: usize,
    pub upstream_accepted: u64,
    pub upstream_rejected: u64,
    pub upstream_accepted_work: TotalWork,
    pub upstream_rejected_work: TotalWork,
    pub upstream_ph_days: PhDays,
    #[serde(flatten)]
    pub stats: MiningStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamDetail {
    pub upstream_id: u32,
    pub upstream: UpstreamInfo,
    pub user_count: usize,
    pub worker_count: usize,
    pub session_count: usize,
    pub disconnected_count: usize,
    pub idle_count: usize,
    pub uptime_secs: u64,
    pub workers: Vec<WorkerDetail>,
    pub sessions: Vec<SessionDetail>,
    #[serde(flatten)]
    pub stats: MiningStats,
}
