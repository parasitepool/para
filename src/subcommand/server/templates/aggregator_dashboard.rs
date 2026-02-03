use super::*;

#[derive(Boilerplate, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct AggregatorDashboardHtml {
    pub(crate) statuses: BTreeMap<String, StatusHtml>,
}

impl PageContent for AggregatorDashboardHtml {
    fn title(&self) -> String {
        "Dashboard".to_string()
    }
}
