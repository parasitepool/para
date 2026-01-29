use {
    super::super::*,
    axum::Extension,
    boilerplate::{Boilerplate, Trusted},
};

#[derive(Boilerplate)]
pub struct DashboardHtml<T: DashboardContent> {
    content: T,
    chain: Chain,
}

impl<T: DashboardContent> DashboardHtml<T> {
    pub fn new(content: T, chain: Chain) -> Self {
        Self { content, chain }
    }

    fn superscript(&self) -> &'static str {
        match self.chain {
            Chain::Mainnet => "",
            Chain::Signet => "signet",
            Chain::Testnet => "testnet",
            Chain::Testnet4 => "testnet4",
            Chain::Regtest => "regtest",
        }
    }
}

pub trait DashboardContent: fmt::Display + 'static {
    fn title(&self) -> &'static str;
}

#[derive(Boilerplate)]
pub struct PoolHtml;

impl DashboardContent for PoolHtml {
    fn title(&self) -> &'static str {
        "Pool"
    }
}

#[derive(Boilerplate)]
pub struct ProxyHtml;

impl DashboardContent for ProxyHtml {
    fn title(&self) -> &'static str {
        "Proxy"
    }
}

pub async fn pool_home(Extension(chain): Extension<Chain>) -> Response {
    dashboard_home(PoolHtml, chain)
}

pub async fn proxy_home(Extension(chain): Extension<Chain>) -> Response {
    dashboard_home(ProxyHtml, chain)
}

fn dashboard_home<T: DashboardContent>(content: T, chain: Chain) -> Response {
    let html = DashboardHtml::new(content, chain);

    #[cfg(feature = "reload")]
    let body = match html.reload_from_path() {
        Ok(reloaded) => reloaded.to_string(),
        Err(_) => html.to_string(),
    };

    #[cfg(not(feature = "reload"))]
    let body = html.to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}
