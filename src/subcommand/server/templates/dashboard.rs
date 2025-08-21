use super::*;

#[derive(Boilerplate, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct DashboardHtml {
    pub(crate) checks: Vec<HealthcheckHtml>,
}

impl PageContent for DashboardHtml {
    fn title(&self) -> String {
        "Dashboard".to_string()
    }
}
