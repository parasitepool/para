use super::*;

#[derive(Boilerplate, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct DashboardHtml {
    pub(crate) statuses: BTreeMap<String, StatusHtml>,
}

impl PageContent for DashboardHtml {
    fn title(&self) -> String {
        "Dashboard".to_string()
    }
}
