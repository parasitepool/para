use {
    super::*,
    boilerplate::Boilerplate,
    html_escaper::{Escape, Trusted},
};

pub(crate) mod home;

#[derive(Boilerplate)]
pub struct PageHtml<T: PageContent> {
    content: T,
}

impl<T> PageHtml<T>
where
    T: PageContent,
{
    pub fn new(content: T) -> Self {
        Self { content }
    }

    fn og_image(&self) -> String {
        "https://parasite.wtf/static/parasite.svg".to_string()
    }
}

pub trait PageContent: Display + 'static {
    fn title(&self) -> String;

    fn page(self) -> PageHtml<Self>
    where
        Self: Sized,
    {
        PageHtml::new(self)
    }
}
