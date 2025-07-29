use super::*;

#[derive(Boilerplate)]
pub(crate) struct HomeHtml {
    pub(crate) stratum_url: String,
}

impl PageContent for HomeHtml {
    fn title(&self) -> String {
        "Parasite".to_string()
    }
}
