use super::*;

pub(crate) fn router(
    metrics: Arc<Metrics>,
    bitcoin_client: Arc<Client>,
    chain: Chain,
    logs: Arc<logs::Logs>,
) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/users", get(users_page))
        .route("/workers", get(workers_page))
        .route("/user/{address}", get(user_page))
        .route("/api/proxy/status", get(status))
        .route("/api/proxy/users", get(users))
        .route("/api/proxy/workers", get(workers))
        .route("/api/proxy/user/{address}", get(user))
        .route("/api/bitcoin/status", get(http_server::bitcoin_status))
        .route("/api/system/status", get(http_server::system_status))
        .route("/ws/logs", get(http_server::ws_logs))
        .route("/static/{*path}", get(http_server::static_assets))
        .with_state(metrics)
        .layer(Extension(bitcoin_client))
        .layer(Extension(chain))
        .layer(Extension(logs))
}

async fn home(Extension(chain): Extension<Chain>) -> Response {
    #[cfg(feature = "reload")]
    let body = {
        use http_server::templates::ReloadedContent;

        let content = ProxyHtml
            .reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| ProxyHtml.to_string());

        let html = DashboardHtml::new(
            ReloadedContent {
                html: content,
                title: "Proxy",
            },
            chain,
        );

        html.reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| html.to_string())
    };

    #[cfg(not(feature = "reload"))]
    let body = DashboardHtml::new(ProxyHtml, chain).to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}

async fn users_page(Extension(chain): Extension<Chain>) -> Response {
    #[cfg(feature = "reload")]
    let body = {
        use http_server::templates::ReloadedContent;

        let content = UsersHtml {
            title: "Proxy | Users",
            api_base: "/api/proxy",
        }
        .reload_from_path()
        .map(|r| r.to_string())
        .unwrap_or_else(|_| {
            UsersHtml {
                title: "Proxy | Users",
                api_base: "/api/proxy",
            }
            .to_string()
        });

        let html = DashboardHtml::new(
            ReloadedContent {
                html: content,
                title: "Proxy | Users",
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
            title: "Proxy | Users",
            api_base: "/api/proxy",
        },
        chain,
    )
    .to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}

async fn workers_page(Extension(chain): Extension<Chain>) -> Response {
    #[cfg(feature = "reload")]
    let body = {
        use http_server::templates::ReloadedContent;

        let content = WorkersHtml {
            title: "Proxy | Workers",
            api_base: "/api/proxy",
        }
        .reload_from_path()
        .map(|r| r.to_string())
        .unwrap_or_else(|_| {
            WorkersHtml {
                title: "Proxy | Workers",
                api_base: "/api/proxy",
            }
            .to_string()
        });

        let html = DashboardHtml::new(
            ReloadedContent {
                html: content,
                title: "Proxy | Workers",
            },
            chain,
        );

        html.reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| html.to_string())
    };

    #[cfg(not(feature = "reload"))]
    let body = DashboardHtml::new(
        WorkersHtml {
            title: "Proxy | Workers",
            api_base: "/api/proxy",
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
            title: "Proxy | User",
            api_base: "/api/proxy",
        }
        .reload_from_path()
        .map(|r| r.to_string())
        .unwrap_or_else(|_| {
            UserHtml {
                title: "Proxy | User",
                api_base: "/api/proxy",
            }
            .to_string()
        });

        let html = DashboardHtml::new(
            ReloadedContent {
                html: content,
                title: "Proxy | User",
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
            title: "Proxy | User",
            api_base: "/api/proxy",
        },
        chain,
    )
    .to_string();

    ([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response()
}

async fn status(State(metrics): State<Arc<Metrics>>) -> Json<ProxyStatus> {
    Json(ProxyStatus {
        endpoint: metrics.metatron.endpoint().to_string(),
        hashrate_1m: metrics.metatron.hashrate_1m(),
        hashrate_5m: metrics.metatron.hashrate_5m(),
        hashrate_15m: metrics.metatron.hashrate_15m(),
        hashrate_1hr: metrics.metatron.hashrate_1hr(),
        hashrate_6hr: metrics.metatron.hashrate_6hr(),
        hashrate_1d: metrics.metatron.hashrate_1d(),
        hashrate_7d: metrics.metatron.hashrate_7d(),
        sps_1m: metrics.metatron.sps_1m(),
        sps_5m: metrics.metatron.sps_5m(),
        sps_15m: metrics.metatron.sps_15m(),
        sps_1hr: metrics.metatron.sps_1hr(),
        users: metrics.metatron.total_users(),
        workers: metrics.metatron.total_workers(),
        disconnected: metrics.metatron.disconnected(),
        idle: metrics.metatron.idle(),
        accepted: metrics.metatron.accepted(),
        rejected: metrics.metatron.rejected(),
        best_ever: metrics.metatron.best_ever(),
        last_share: metrics
            .metatron
            .last_share()
            .map(|time| time.elapsed().as_secs()),
        total_work: metrics.metatron.total_work(),
        uptime_secs: metrics.metatron.uptime().as_secs(),
        upstream_endpoint: metrics.upstream.endpoint().to_string(),
        upstream_connected: metrics.upstream.is_connected(),
        upstream_ping: metrics.upstream.ping_ms().await,
        upstream_difficulty: metrics.upstream.difficulty().await,
        upstream_username: metrics.upstream.username().clone(),
        upstream_enonce1: metrics.upstream.enonce1().clone(),
        upstream_enonce2_size: metrics.upstream.enonce2_size(),
        upstream_version_mask: metrics.upstream.version_mask(),
        upstream_accepted: metrics.upstream.accepted(),
        upstream_rejected: metrics.upstream.rejected(),
        upstream_filtered: metrics.upstream.filtered(),
    })
}

async fn users(State(metrics): State<Arc<Metrics>>) -> Json<Vec<String>> {
    Json(
        metrics
            .metatron
            .users()
            .iter()
            .map(|entry| entry.key().to_string())
            .collect(),
    )
}

async fn workers(State(metrics): State<Arc<Metrics>>) -> Json<Vec<WorkerListDetail>> {
    Json(
        metrics
            .metatron
            .users()
            .iter()
            .flat_map(|user| {
                let address = user.key().to_string();
                user.workers
                    .iter()
                    .map(|worker| WorkerListDetail {
                        user: address.clone(),
                        name: worker.workername().to_string(),
                        instances: worker.instance_count(),
                        hashrate_5m: worker.hashrate_5m(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
    )
}

async fn user(
    State(metrics): State<Arc<Metrics>>,
    Path(address): Path<Address<NetworkUnchecked>>,
) -> ServerResult<Response> {
    let address = address.assume_checked();

    let user = metrics
        .metatron
        .users()
        .get(&address)
        .ok_or_not_found(|| format!("User {address}"))?;

    Ok(Json(UserDetail {
        address: user.address.to_string(),
        hashrate_1m: user.hashrate_1m(),
        hashrate_5m: user.hashrate_5m(),
        hashrate_15m: user.hashrate_15m(),
        hashrate_1hr: user.hashrate_1hr(),
        hashrate_6hr: user.hashrate_6hr(),
        hashrate_1d: user.hashrate_1d(),
        hashrate_7d: user.hashrate_7d(),
        sps_1m: user.sps_1m(),
        sps_5m: user.sps_5m(),
        sps_15m: user.sps_15m(),
        sps_1hr: user.sps_1hr(),
        accepted: user.accepted(),
        rejected: user.rejected(),
        best_ever: user.best_ever(),
        last_share: user.last_share().map(|time| time.elapsed().as_secs()),
        total_work: user.total_work(),
        authorized: user.authorized,
        workers: user
            .workers()
            .map(|worker| WorkerDetail {
                name: worker.workername().to_string(),
                instances: worker.instance_count(),
                hashrate_1m: worker.hashrate_1m(),
                hashrate_5m: worker.hashrate_5m(),
                hashrate_15m: worker.hashrate_15m(),
                hashrate_1hr: worker.hashrate_1hr(),
                hashrate_6hr: worker.hashrate_6hr(),
                hashrate_1d: worker.hashrate_1d(),
                hashrate_7d: worker.hashrate_7d(),
                sps_1m: worker.sps_1m(),
                sps_5m: worker.sps_5m(),
                sps_15m: worker.sps_15m(),
                sps_1hr: worker.sps_1hr(),
                accepted: worker.accepted(),
                rejected: worker.rejected(),
                best_ever: worker.best_ever(),
                last_share: worker.last_share().map(|time| time.elapsed().as_secs()),
                total_work: worker.total_work(),
            })
            .collect(),
    })
    .into_response())
}
