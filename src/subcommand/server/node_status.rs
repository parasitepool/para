use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, ToSchema)]
pub struct NodeStatus {
    pub cpu_usage_percent: f64,
    pub disk_usage_percent: f64,
    pub memory_usage_percent: f64,
    pub uptime: u64,
    pub blockheight: Option<i32>,
    pub users: Option<u64>,
    pub workers: Option<u64>,
    pub hashrate: Option<ckpool::HashRate>,
    pub sps: Option<f64>,
    pub total_work: Option<f64>,
    pub best_share: Option<u64>,
    pub accepted: Option<u64>,
    pub rejected: Option<u64>,
}
