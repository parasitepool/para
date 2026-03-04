use super::*;

#[derive(Boilerplate)]
pub(crate) struct DashboardHtml<T: DashboardContent> {
    content: T,
    chain: Chain,
}

impl<T: DashboardContent> DashboardHtml<T> {
    pub(crate) fn new(content: T, chain: Chain) -> Self {
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

pub(crate) trait DashboardContent: fmt::Display + 'static {
    fn title(&self) -> &'static str;
}

#[derive(Boilerplate)]
pub(crate) struct PoolHtml;

impl DashboardContent for PoolHtml {
    fn title(&self) -> &'static str {
        "Pool"
    }
}

#[derive(Boilerplate)]
pub(crate) struct ProxyHtml;

impl DashboardContent for ProxyHtml {
    fn title(&self) -> &'static str {
        "Proxy"
    }
}

#[derive(Boilerplate)]
pub(crate) struct RouterHtml;

impl DashboardContent for RouterHtml {
    fn title(&self) -> &'static str {
        "Router"
    }
}

#[derive(Boilerplate)]
pub(crate) struct UpstreamHtml;

impl DashboardContent for UpstreamHtml {
    fn title(&self) -> &'static str {
        "Router | Upstream"
    }
}

#[derive(Boilerplate)]
pub(crate) struct UsersHtml {
    pub(crate) title: &'static str,
    pub(crate) api_base: &'static str,
}

impl DashboardContent for UsersHtml {
    fn title(&self) -> &'static str {
        self.title
    }
}

#[derive(Boilerplate)]
pub(crate) struct UserHtml {
    pub(crate) title: &'static str,
    pub(crate) api_base: &'static str,
}

impl DashboardContent for UserHtml {
    fn title(&self) -> &'static str {
        self.title
    }
}

#[cfg(feature = "reload")]
pub(crate) struct ReloadedContent {
    pub(crate) html: String,
    pub(crate) title: &'static str,
}

#[cfg(feature = "reload")]
impl fmt::Display for ReloadedContent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.html)
    }
}

#[cfg(feature = "reload")]
impl DashboardContent for ReloadedContent {
    fn title(&self) -> &'static str {
        self.title
    }
}

pub(crate) fn render_page(content: impl DashboardContent + Boilerplate, chain: Chain) -> Response {
    #[cfg(feature = "reload")]
    let body = {
        let title = content.title();
        let html = content
            .reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| content.to_string());

        let dashboard = DashboardHtml::new(ReloadedContent { html, title }, chain);

        dashboard
            .reload_from_path()
            .map(|reloaded| reloaded.to_string())
            .unwrap_or_else(|_| dashboard.to_string())
    };

    #[cfg(not(feature = "reload"))]
    let body = DashboardHtml::new(content, chain).to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}
