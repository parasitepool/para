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
        hashrate_1m: stats.hashrate_1m(now),
        hashrate_5m: stats.hashrate_5m(now),
        hashrate_15m: stats.hashrate_15m(now),
        hashrate_1hr: stats.hashrate_1hr(now),
        hashrate_6hr: stats.hashrate_6hr(now),
        hashrate_1d: stats.hashrate_1d(now),
        hashrate_7d: stats.hashrate_7d(now),
        sps_1m: stats.sps_1m(now),
        sps_5m: stats.sps_5m(now),
        sps_15m: stats.sps_15m(now),
        sps_1hr: stats.sps_1hr(now),
        users: metatron.total_users(),
        workers: metatron.total_workers(),
        sessions: metatron.total_sessions(),
        disconnected: metatron.disconnected(),
        idle: metatron.idle(),
        accepted_shares: stats.accepted_shares,
        rejected_shares: stats.rejected_shares,
        blocks: metatron.total_blocks(),
        best_ever: stats.best_ever,
        last_share: stats
            .last_share
            .map(|time| now.duration_since(time).as_secs()),
        accepted_work: stats.accepted_work,
        rejected_work: stats.rejected_work,
        ph_days: stats.accepted_work.into(),
        uptime_secs: metatron.uptime().as_secs(),
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
                sessions: worker.session_count(),
                hashrate_1m: stats.hashrate_1m(now),
                hashrate_5m: stats.hashrate_5m(now),
                hashrate_15m: stats.hashrate_15m(now),
                hashrate_1hr: stats.hashrate_1hr(now),
                hashrate_6hr: stats.hashrate_6hr(now),
                hashrate_1d: stats.hashrate_1d(now),
                hashrate_7d: stats.hashrate_7d(now),
                sps_1m: stats.sps_1m(now),
                sps_5m: stats.sps_5m(now),
                sps_15m: stats.sps_15m(now),
                sps_1hr: stats.sps_1hr(now),
                accepted_shares: stats.accepted_shares,
                rejected_shares: stats.rejected_shares,
                accepted_work: stats.accepted_work,
                rejected_work: stats.rejected_work,
                ph_days: stats.accepted_work.into(),
                best_ever: stats.best_ever,
                last_share: stats
                    .last_share
                    .map(|time| now.duration_since(time).as_secs()),
            }
        })
        .collect();

    let user_stats = user.snapshot();

    Ok(Json(UserDetail {
        address: user.address.to_string(),
        hashrate_1m: user_stats.hashrate_1m(now),
        hashrate_5m: user_stats.hashrate_5m(now),
        hashrate_15m: user_stats.hashrate_15m(now),
        hashrate_1hr: user_stats.hashrate_1hr(now),
        hashrate_6hr: user_stats.hashrate_6hr(now),
        hashrate_1d: user_stats.hashrate_1d(now),
        hashrate_7d: user_stats.hashrate_7d(now),
        sps_1m: user_stats.sps_1m(now),
        sps_5m: user_stats.sps_5m(now),
        sps_15m: user_stats.sps_15m(now),
        sps_1hr: user_stats.sps_1hr(now),
        accepted_shares: user_stats.accepted_shares,
        rejected_shares: user_stats.rejected_shares,
        best_ever: user_stats.best_ever,
        last_share: user_stats
            .last_share
            .map(|time| now.duration_since(time).as_secs()),
        accepted_work: user_stats.accepted_work,
        rejected_work: user_stats.rejected_work,
        ph_days: user_stats.accepted_work.into(),
        sessions: user.session_count(),
        authorized: user.authorized,
        workers,
    })
    .into_response())
}
