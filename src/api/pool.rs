use super::*;

pub(crate) fn router(
    metatron: Arc<Metatron>,
    bitcoin_client: Arc<BitcoindClient>,
    chain: Chain,
    logs: Arc<logs::Logs>,
) -> axum::Router {
    axum::Router::new()
        .route("/", get(home))
        .route("/users", get(users_page))
        .route("/user/{address}", get(user_page))
        .route("/api/pool/status", get(status))
        .route("/api/pool/users", get(users))
        .route("/api/pool/user/{address}", get(user))
        .route("/api/bitcoin/status", get(http_server::bitcoin_status))
        .route("/api/system/status", get(http_server::system_status))
        .route("/ws/logs", get(http_server::ws_logs))
        .route("/static/{*path}", get(http_server::static_assets))
        .with_state(metatron)
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
}

async fn home(Extension(chain): Extension<Chain>) -> Response {
    #[cfg(feature = "reload")]
    let body = {
        use http_server::templates::ReloadedContent;

        let content = PoolHtml
            .reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| PoolHtml.to_string());

        let html = DashboardHtml::new(
            ReloadedContent {
                html: content,
                title: "Pool",
            },
            chain,
        );

        html.reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| html.to_string())
    };

    #[cfg(not(feature = "reload"))]
    let body = DashboardHtml::new(PoolHtml, chain).to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}

async fn users_page(Extension(chain): Extension<Chain>) -> Response {
    #[cfg(feature = "reload")]
    let body = {
        use http_server::templates::ReloadedContent;

        let content = UsersHtml {
            title: "Pool | Users",
            api_base: "/api/pool",
        }
        .reload_from_path()
        .map(|r| r.to_string())
        .unwrap_or_else(|_| {
            UsersHtml {
                title: "Pool | Users",
                api_base: "/api/pool",
            }
            .to_string()
        });

        let html = DashboardHtml::new(
            ReloadedContent {
                html: content,
                title: "Pool | Users",
            },
            chain,
        );

        html.reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| html.to_string())
    };

    #[cfg(not(feature = "reload"))]
    let body = DashboardHtml::new(
        UsersHtml {
            title: "Pool | Users",
            api_base: "/api/pool",
        },
        chain,
    )
    .to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}

async fn user_page(Extension(chain): Extension<Chain>) -> Response {
    #[cfg(feature = "reload")]
    let body = {
        use http_server::templates::ReloadedContent;

        let content = UserHtml {
            title: "Pool | User",
            api_base: "/api/pool",
        }
        .reload_from_path()
        .map(|r| r.to_string())
        .unwrap_or_else(|_| {
            UserHtml {
                title: "Pool | User",
                api_base: "/api/pool",
            }
            .to_string()
        });

        let html = DashboardHtml::new(
            ReloadedContent {
                html: content,
                title: "Pool | User",
            },
            chain,
        );

        html.reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| html.to_string())
    };

    #[cfg(not(feature = "reload"))]
    let body = DashboardHtml::new(
        UserHtml {
            title: "Pool | User",
            api_base: "/api/pool",
        },
        chain,
    )
    .to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}

async fn status(State(metatron): State<Arc<Metatron>>) -> Json<PoolStatus> {
    let now = Instant::now();
    let stats = metatron.snapshot();

    Json(PoolStatus {
        endpoint: metatron.endpoint().to_string(),
        user_count: metatron.total_users(),
        worker_count: metatron.total_workers(),
        block_count: metatron.total_blocks(),
        session_count: metatron.total_sessions(),
        disconnected_count: metatron.total_disconnected(),
        idle_count: metatron.total_idle(),
        uptime_secs: metatron.uptime().as_secs(),
        stats: MiningStats::from_snapshot(&stats, now),
    })
}

async fn users(State(metatron): State<Arc<Metatron>>) -> Json<Vec<String>> {
    Json(
        metatron
            .users()
            .iter()
            .map(|entry| entry.key().to_string())
            .collect(),
    )
}

async fn user(
    State(metatron): State<Arc<Metatron>>,
    Path(address): Path<Address<NetworkUnchecked>>,
) -> ServerResult<Response> {
    let address = address.assume_checked();

    let now = Instant::now();

    let user = metatron
        .users()
        .get(&address)
        .ok_or_not_found(|| format!("User {address}"))?;

    let workers: Vec<WorkerDetail> = user
        .workers()
        .map(|worker| {
            let stats = worker.snapshot();
            WorkerDetail {
                name: worker.workername().to_string(),
                session_count: worker.session_count(),
                stats: MiningStats::from_snapshot(&stats, now),
            }
        })
        .collect();

    let user_stats = user.snapshot();

    Ok(Json(UserDetail {
        address: user.address.to_string(),
        session_count: user.session_count(),
        authorized_at: user.authorized,
        workers,
        stats: MiningStats::from_snapshot(&user_stats, now),
    })
    .into_response())
}
