use super::*;

#[derive(Boilerplate, Debug, Deserialize, Serialize, PartialEq)]
pub struct HealthcheckHtml {
    pub cpu_usage_percent: f64,
    pub disk_usage_percent: f64,
    pub memory_usage_percent: f64,
    pub uptime: u64,
}

impl PageContent for HealthcheckHtml {
    fn title(&self) -> String {
        "Healthcheck".to_string()
    }
}

impl HealthcheckHtml {
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
