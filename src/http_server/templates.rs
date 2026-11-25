use super::*;

#[derive(Boilerplate)]
pub(crate) struct DashboardHtml<T: DashboardContent> {
    content: T,
    chain: Chain,
    auth: NavbarAuth,
}

impl<T: DashboardContent> DashboardHtml<T> {
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

    fn is_router(&self) -> bool {
        false
    }
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

    fn is_router(&self) -> bool {
        true
    }
}

#[derive(Boilerplate)]
pub(crate) struct ReviewHtml;

impl DashboardContent for ReviewHtml {
    fn title(&self) -> &'static str {
        "Router | Review"
    }

    fn is_router(&self) -> bool {
        true
    }
}

#[derive(Boilerplate)]
pub(crate) struct TapsHtml;

impl DashboardContent for TapsHtml {
    fn title(&self) -> &'static str {
        "Router | Taps"
    }

    fn is_router(&self) -> bool {
        true
    }
}

#[derive(Boilerplate)]
pub(crate) struct TapHtml;

impl DashboardContent for TapHtml {
    fn title(&self) -> &'static str {
        "Router | Tap"
    }

    fn is_router(&self) -> bool {
        true
    }
}

#[derive(Boilerplate)]
pub(crate) struct LoginHtml;

#[derive(Boilerplate)]
pub(crate) struct OrderHtml;

impl DashboardContent for OrderHtml {
    fn title(&self) -> &'static str {
        "Router | Order"
    }

    fn is_router(&self) -> bool {
        true
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

    fn is_router(&self) -> bool {
        self.api_base == "/api/router"
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

    fn is_router(&self) -> bool {
        self.api_base == "/api/router"
    }
}

#[cfg(feature = "reload")]
pub(crate) struct ReloadedContent {
    pub(crate) html: String,
    pub(crate) title: &'static str,
    pub(crate) is_router: bool,
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

    fn is_router(&self) -> bool {
        self.is_router
    }
}

pub(crate) fn render_page(
    content: impl DashboardContent + Boilerplate,
    chain: Chain,
    auth: NavbarAuth,
) -> Response {
    if auth.enabled && !auth.authed {
        return Redirect::to(&format!("/login?next={}", urlencoding::encode(&auth.next)))
            .into_response();
    }

    #[cfg(feature = "reload")]
    let body = {
        let title = content.title();
        let is_router = content.is_router();
        let html = content
            .reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| content.to_string());

        let dashboard = DashboardHtml {
            content: ReloadedContent {
                html,
                title,
                is_router,
            },
            chain,
            auth,
        };

        dashboard
            .reload_from_path()
            .map(|reloaded| reloaded.to_string())
            .unwrap_or_else(|_| dashboard.to_string())
    };

    #[cfg(not(feature = "reload"))]
    let body = DashboardHtml {
        content,
        chain,
        auth,
    }
    .to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}
