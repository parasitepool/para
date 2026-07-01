use super::*;

pub(crate) struct Aggregator;

impl Aggregator {
    pub(crate) fn init(
        config: Arc<ServerConfig>,
        database: Option<Database>,
    ) -> Result<axum::Router> {
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
            .build()?;

        let cache = Arc::new(Cache::new(client.clone(), config.clone(), database));

        let router = axum::Router::new()
            .route("/aggregator/blockheight", get(blockheight))
            .route("/aggregator/pool/pool.status", get(pool_status))
            .route("/aggregator/users/{address}", get(user_status))
            .route("/aggregator/users", get(users))
            .layer(from_extractor::<ApiAuth>())
            .layer(Extension(cache))
            .layer(Extension(client))
            .layer(Extension(config));

        Ok(router)
    }
}

/// Get aggregated pool status across all nodes
#[utoipa::path(
    get,
    path = "/aggregator/pool/pool.status",
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Aggregated pool status in ckpool format", content_type = "text/plain", body = String),
        (status = 404, description = "Pool status not available"),
    ),
    tag = "aggregator"
)]
pub(crate) async fn pool_status(Extension(cache): Extension<Arc<Cache>>) -> ServerResult<Response> {
    Ok(cache
        .pool_status()
        .await?
        .ok_or_not_found(|| "Pool status")?
        .to_string()
        .into_response())
}

/// Get aggregated user status across all nodes
#[utoipa::path(
    get,
    path = "/aggregator/users/{address}",
    security(("api_token" = [])),
    params(
        ("address" = String, Path, description = "BTC address")
    ),
    responses(
        (status = 200, description = "Aggregated user status", body = ckpool::User),
        (status = 404, description = "User not found"),
    ),
    tag = "aggregator"
)]
pub(crate) async fn user_status(
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

/// List all users across all aggregator nodes
#[utoipa::path(
    get,
    path = "/aggregator/users",
    security(("api_token" = [])),
    responses(
        (status = 200, description = "List of all users", body = Vec<String>),
        (status = 404, description = "Users not available"),
    ),
    tag = "aggregator"
)]
pub(crate) async fn users(Extension(cache): Extension<Arc<Cache>>) -> ServerResult<Response> {
    Ok(Json(cache.users().await?.ok_or_not_found(|| "Users")?).into_response())
}

/// Get minimum blockheight across all aggregator nodes
#[utoipa::path(
    get,
    path = "/aggregator/blockheight",
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Minimum blockheight across nodes", body = i32),
    ),
    tag = "aggregator"
)]
pub(crate) async fn blockheight(
    Extension(client): Extension<Client>,
    Extension(config): Extension<Arc<ServerConfig>>,
) -> ServerResult<Response> {
    let mut nodes = config.nodes();
    if let Some(sync_endpoint) = config.sync_endpoint() {
        nodes.push(Url::from_str(&sync_endpoint).map_err(|err| anyhow!(err))?);
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
                let status: NodeStatus =
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
