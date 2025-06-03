use super::*;

#[derive(Boilerplate)]
pub(crate) struct HealthcheckHtml {
    pub(crate) disk_usage_percent: String,
    pub(crate) memory_usage_percent: String,
    pub(crate) cpu_usage_percent: String,
    pub(crate) uptime: String,
}

impl PageContent for HealthcheckHtml {
    fn title(&self) -> String {
        "Healthcheck".to_string()
    }
}
