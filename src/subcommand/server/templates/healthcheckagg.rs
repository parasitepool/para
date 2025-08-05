use super::*;

#[derive(Boilerplate, Debug, Deserialize, Serialize, PartialEq)]
pub(crate) struct HealthcheckaggHtml {
    pub(crate) checks: Vec<HealthcheckHtml>,
}

impl PageContent for HealthcheckaggHtml {
    fn title(&self) -> String {
        "Healthchecks".to_string()
    }
}
