use super::*;

#[derive(Boilerplate, Debug, Deserialize, Serialize, PartialEq)]
pub struct StatusHtml {
    pub cpu_usage_percent: f64,
    pub disk_usage_percent: f64,
    pub memory_usage_percent: f64,
    pub uptime: u64,

    pub users: Option<u64>,
    pub workers: Option<u64>,

    pub hashrate: Option<ckpool::HashRate>,
    pub sps: Option<f64>,
    pub total_work: Option<f64>,
    pub best_share: Option<u64>,

    pub accepted: Option<u64>,
    pub rejected: Option<u64>,
}

impl PageContent for StatusHtml {
    fn title(&self) -> String {
        "Status".to_string()
    }
}

impl StatusHtml {
    pub fn display_cpu_usage(&self) -> String {
        format!("{:.2}", self.cpu_usage_percent)
    }

    pub fn display_disk_usage(&self) -> String {
        format!("{:.2}", self.disk_usage_percent)
    }

    pub fn display_memory_usage(&self) -> String {
        format!("{:.2}", self.memory_usage_percent)
    }

    pub fn display_uptime(&self) -> String {
        format_uptime(self.uptime)
    }
}
