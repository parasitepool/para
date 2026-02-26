use {
    super::*,
    axum::extract::{Path, State},
    http_server::{
        self,
        error::{OptionExt, ServerResult},
        templates::{DashboardHtml, PoolHtml, ProxyHtml, UserHtml, UsersHtml},
    },
};

pub mod pool;
pub mod proxy;
pub mod router;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStatus {
    pub endpoint: String,
    pub users: usize,
    pub workers: usize,
    pub sessions: usize,
    pub disconnected: usize,
    pub idle: usize,
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
    pub blocks: u64,
    pub best_ever: Option<Difficulty>,
    pub last_share: Option<u64>,
    pub accepted_work: TotalWork,
    pub rejected_work: TotalWork,
    pub ph_days: PhDays,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyStatus {
    pub endpoint: String,
    pub users: usize,
    pub workers: usize,
    pub sessions: usize,
    pub disconnected: usize,
    pub idle: usize,
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
    pub best_ever: Option<Difficulty>,
    pub last_share: Option<u64>,
    pub accepted_work: TotalWork,
    pub rejected_work: TotalWork,
    pub ph_days: PhDays,
    pub uptime_secs: u64,
    pub upstream_endpoint: String,
    pub upstream_connected: bool,
    pub upstream_ping: u128,
    pub upstream_difficulty: Difficulty,
    pub upstream_username: Username,
    pub upstream_enonce1: Extranonce,
    pub upstream_enonce2_size: usize,
    pub upstream_version_mask: Option<Version>,
    pub upstream_accepted: u64,
    pub upstream_rejected: u64,
    pub upstream_filtered: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDetail {
    pub address: String,
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
    pub best_ever: Option<Difficulty>,
    pub last_share: Option<u64>,
    pub accepted_work: TotalWork,
    pub rejected_work: TotalWork,
    pub ph_days: PhDays,
    pub sessions: usize,
    pub authorized: u64,
    pub workers: Vec<WorkerDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerDetail {
    pub name: String,
    pub sessions: usize,
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
    pub best_ever: Option<Difficulty>,
    pub last_share: Option<u64>,
    pub accepted_work: TotalWork,
    pub rejected_work: TotalWork,
    pub ph_days: PhDays,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterStatus {
    pub slots: Vec<SlotStatus>,
    pub total_sessions: usize,
    pub total_hashrate_1m: HashRate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotStatus {
    pub index: usize,
    pub endpoint: String,
    pub username: String,
    pub connected: bool,
    pub hashrate_1m: HashRate,
    pub sessions: Vec<SlotSessionStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotSessionStatus {
    pub id: u64,
    pub worker_name: String,
    pub hashrate_1m: HashRate,
}

pub type BitcoinStatus = http_server::BitcoinStatus;
pub type SystemStatus = http_server::SystemStatus;
