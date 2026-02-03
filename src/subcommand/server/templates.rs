use {
    super::*,
    boilerplate::{Boilerplate, Trusted},
};

pub(crate) mod aggregator_dashboard;
pub(crate) mod home;
pub(crate) mod payouts;
pub(crate) mod simulate_payouts;
pub(crate) mod status;

#[derive(Boilerplate)]
pub struct PageHtml<T: PageContent> {
    content: T,
    domain: String,
}

impl<T> PageHtml<T>
where
    T: PageContent,
{
    pub fn new(content: T, domain: String) -> Self {
        Self { content, domain }
    }

    fn og_image(&self) -> String {
        format!("https://{}/static/parasite.svg", self.domain)
    }
}

pub trait PageContent: fmt::Display + 'static {
    fn title(&self) -> String;

    fn page(self, domain: String) -> PageHtml<Self>
    where
        Self: Sized,
    {
        PageHtml::new(self, domain)
    }
}

pub fn format_sats(sats: i64) -> String {
    if sats >= 100_000_000 {
        format!("{:.3} BTC", sats as f64 / 100_000_000.0)
    } else if sats >= 1_000_000 {
        format!("{:.2}M sats", sats as f64 / 1_000_000.0)
    } else if sats >= 1_000 {
        format!("{:.2}K sats", sats as f64 / 1_000.0)
    } else {
        format!("{} sats", sats)
    }
}
