use {
    super::*,
    axum::extract::{Path, State},
    boilerplate::Boilerplate,
    http_server::{
        self,
        error::{OptionExt, ServerResult},
        templates::{DashboardHtml, PoolHtml, ProxyHtml},
    },
};

pub mod pool;
pub mod proxy;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStatus {
    pub endpoint: String,
    pub users: usize,
    pub workers: usize,
    pub connections: u64,
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
    pub accepted: u64,
    pub rejected: u64,
    pub blocks: u64,
    pub best_ever: Option<Difficulty>,
    pub last_share: Option<u64>,
    pub total_work: f64,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyStatus {
    pub endpoint: String,
    pub users: usize,
    pub workers: usize,
    pub connections: u64,
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
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: Option<Difficulty>,
    pub last_share: Option<u64>,
    pub total_work: f64,
    pub uptime_secs: u64,
    pub upstream_endpoint: String,
    pub upstream_connected: bool,
    pub upstream_difficulty: f64,
    pub upstream_username: Username,
    pub upstream_enonce1: Extranonce,
    pub upstream_enonce2_size: usize,
    pub upstream_version_mask: Option<Version>,
    pub upstream_accepted: u64,
    pub upstream_rejected: u64,
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
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: Option<Difficulty>,
    pub last_share: Option<u64>,
    pub total_work: f64,
    pub authorized: u64,
    pub workers: Vec<WorkerDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerDetail {
    pub name: String,
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
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: Option<Difficulty>,
    pub last_share: Option<u64>,
    pub total_work: f64,
}

pub type BitcoinStatus = http_server::BitcoinStatus;
pub type SystemStatus = http_server::SystemStatus;
