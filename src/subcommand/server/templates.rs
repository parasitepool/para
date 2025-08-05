use {
    super::*,
    boilerplate::Boilerplate,
    html_escaper::{Escape, Trusted},
};

pub(crate) mod healthcheck;
pub(crate) mod healthcheckagg;
pub(crate) mod home;

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
