use super::*;

#[derive(Boilerplate, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct DashboardHtml {
    pub(crate) healthchecks: BTreeMap<String, HealthcheckHtml>,
}

impl PageContent for DashboardHtml {
    fn title(&self) -> String {
        "Dashboard".to_string()
    }
}
