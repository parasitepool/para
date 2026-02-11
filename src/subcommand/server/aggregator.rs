use super::*;

#[derive(Serialize)]
struct AggregatorNode {
    hostname: String,
    #[serde(flatten)]
    status: StatusHtml,
}

#[derive(Serialize)]
struct AggregatorStatuses {
    aggregator: Option<AggregatorNode>,
    nodes: BTreeMap<String, StatusHtml>,
}

pub(crate) struct Aggregator;

impl Aggregator {
    pub(crate) fn init(config: Arc<ServerConfig>) -> Result<Router> {
        let mut headers = header::HeaderMap::new();
        if let Some(token) = config.api_token() {
            headers.insert(
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }

        headers.insert(header::ACCEPT, HeaderValue::from_str("application/json")?);

        let client = ClientBuilder::new()
            .default_headers(headers)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .pool_idle_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(3)
            .use_rustls_tls()
            .build()?;

        let cache = Arc::new(Cache::new(client.clone(), config.clone()));

        let mut router = Router::new()
            .route("/aggregator/blockheight", get(Self::blockheight))
            .route("/aggregator/pool/pool.status", get(Self::pool_status))
            .route("/aggregator/users/{address}", get(Self::user_status))
            .route("/aggregator/users", get(Self::users));

        router = if let Some(token) = config.api_token() {
            router.layer(bearer_auth(token))
        } else {
            router
        }
        .route(
            "/aggregator/dashboard",
            Server::with_auth(config.clone(), get(Self::dashboard)),
        )
        .route(
            "/aggregator/api/statuses",
            Server::with_auth(config.clone(), get(Self::statuses)),
        )
        .layer(Extension(cache))
        .layer(Extension(client))
        .layer(Extension(config));

        Ok(router)
    }

    async fn pool_status(Extension(cache): Extension<Arc<Cache>>) -> ServerResult<Response> {
        Ok(cache
            .pool_status()
            .await?
            .ok_or_not_found(|| "Pool status")?
            .to_string()
            .into_response())
    }

    async fn user_status(
        Path(address): Path<String>,
        Extension(cache): Extension<Arc<Cache>>,
    ) -> ServerResult<Response> {
        Ok(Json(
            cache
                .user_status(address.clone())
                .await?
                .ok_or_not_found(|| format!("User {address}"))?,
        )
        .into_response())
    }

    async fn users(Extension(cache): Extension<Arc<Cache>>) -> ServerResult<Response> {
        Ok(Json(cache.users().await?.ok_or_not_found(|| "Users")?).into_response())
    }

    async fn blockheight(
        Extension(client): Extension<Client>,
        Extension(config): Extension<Arc<ServerConfig>>,
    ) -> ServerResult<Response> {
        let mut nodes = config.nodes();
        if let Some(aggregator_node) = config.aggregator_node() {
            nodes.push(aggregator_node);
        }
        let admin_token = config.admin_token();

        let fetches = nodes.iter().map(|url| {
            let client = client.clone();
            async move {
                async {
                    let mut request_builder = client.get(url.join("/status")?);

                    if let Some(token) = admin_token {
                        request_builder = request_builder.bearer_auth(token);
                    }

                    let resp = request_builder.send().await?;
                    let status: Status =
                        serde_json::from_str(&resp.text().await?).map_err(|err| anyhow!(err))?;

                    Ok::<_, Error>(status.blockheight)
                }
                .await
            }
        });

        let results: Vec<Result<Option<i32>>> = futures::future::join_all(fetches).await;

        let blockheights: Vec<i32> = results
            .into_iter()
            .filter_map(|r| r.ok())
            .flatten()
            .collect();

        let min_blockheight = blockheights.into_iter().min();

        Ok(Json(min_blockheight.unwrap_or(0)).into_response())
    }

    async fn fetch_node_statuses(
        client: &Client,
        config: &ServerConfig,
    ) -> Vec<(String, bool, Result<Status>)> {
        let mut nodes = config.nodes();

        let aggregator_node = config.aggregator_node();

        if let Some(ref url) = aggregator_node {
            nodes.push(url.clone());
        }

        let admin_token = config.admin_token();

        let fetches = nodes.iter().map(|url| {
            let client = client.clone();
            let is_aggregator = aggregator_node
                .as_ref()
                .is_some_and(|agg| agg.host_str() == url.host_str());
            async move {
                let result = async {
                    let mut request_builder = client.get(url.join("/status")?);

                    if let Some(token) = admin_token {
                        request_builder = request_builder.bearer_auth(token);
                    }

                    let resp = request_builder.send().await?;

                    serde_json::from_str(&resp.text().await?).map_err(|err| anyhow!(err))
                }
                .await;

                (
                    url.host_str().unwrap_or("unknown").to_string(),
                    is_aggregator,
                    result,
                )
            }
        });

        futures::future::join_all(fetches).await
    }

    async fn statuses(
        Extension(client): Extension<Client>,
        Extension(config): Extension<Arc<ServerConfig>>,
    ) -> ServerResult<Response> {
        let results = Self::fetch_node_statuses(&client, &config).await;

        let mut aggregator = None;
        let mut nodes = BTreeMap::new();

        for (hostname, is_aggregator, result) in results {
            if let Ok(status) = result {
                if is_aggregator {
                    aggregator = Some(AggregatorNode {
                        hostname: hostname.clone(),
                        status,
                    });
                } else {
                    nodes.insert(hostname, status);
                }
            }
        }

        Ok(Json(AggregatorStatuses { aggregator, nodes }).into_response())
    }

    async fn dashboard() -> ServerResult<Response> {
        #[cfg(feature = "reload")]
        let body = AggregatorDashboardHtml
            .reload_from_path()
            .map(|r| r.to_string())
            .unwrap_or_else(|_| AggregatorDashboardHtml.to_string());

        #[cfg(not(feature = "reload"))]
        let body = AggregatorDashboardHtml.to_string();

        Ok(([(CONTENT_TYPE, "text/html;charset=utf-8")], body).into_response())
    }
}
