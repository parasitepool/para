use {super::*, crate::router::Router};

pub(crate) fn router(
    router: Arc<Router>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
    logs: Arc<logs::Logs>,
) -> axum::Router {
    axum::Router::new()
        .route("/", get(home))
        .route("/users", get(users_page))
        .route("/user/{address}", get(user_page))
        .route("/api/proxy/status", get(status))
        .route("/api/proxy/users", get(users))
        .route("/api/proxy/user/{address}", get(user))
        .with_state(router)
        .merge(common_routes())
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
}

async fn users(State(router): State<Arc<Router>>) -> Json<Vec<String>> {
    pool::users(State(router.metatron())).await
}

async fn user(
    State(router): State<Arc<Router>>,
    path: Path<Address<NetworkUnchecked>>,
) -> ServerResult<Response> {
    pool::user(State(router.metatron()), path).await
}

async fn home(Extension(chain): Extension<Chain>) -> Response {
    render_page(ProxyHtml, chain)
}

async fn users_page(Extension(chain): Extension<Chain>) -> Response {
    render_page(
        UsersHtml {
            title: "Proxy | Users",
            api_base: "/api/proxy",
        },
        chain,
    )
}

async fn user_page(Extension(chain): Extension<Chain>) -> Response {
    render_page(
        UserHtml {
            title: "Proxy | User",
            api_base: "/api/proxy",
        },
        chain,
    )
}

async fn status(State(router): State<Arc<Router>>) -> ServerResult<Response> {
    let now = Instant::now();
    let metatron = router.metatron();
    let orders = router.orders();
    let order = orders
        .first()
        .ok_or_not_found(|| "Proxy upstream".to_string())?;
    let upstream = order
        .upstream()
        .ok_or_not_found(|| "Proxy upstream".to_string())?;

    Ok(Json(ProxyStatus {
        uptime_secs: metatron.uptime().as_secs(),
        upstream: UpstreamInfo::from_upstream(&upstream, &metatron, now),
        downstream: DownstreamInfo::from_metatron(&metatron, now),
    })
    .into_response())
}
