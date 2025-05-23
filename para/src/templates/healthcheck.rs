use super::*;

#[derive(Boilerplate)]
pub(crate) struct HealthcheckHtml {
    pub(crate) disk_usage_percent: String,
    pub(crate) memory_usage_percent: String,
    pub(crate) cpu_usage_percent: String,
    pub(crate) uptime_seconds: u64,
}

impl PageContent for HealthcheckHtml {
    fn title(&self) -> String {
        "Parasite - Health Check".to_string()
    }
}
