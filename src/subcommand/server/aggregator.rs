use super::*;

pub(crate) struct Aggregator;

impl Aggregator {
    pub(crate) fn init(config: Arc<ServerConfig>) -> Result<Router> {
        let mut headers = header::HeaderMap::new();
        if let Some(token) = config.api_token() {
            headers.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }

        let client = ClientBuilder::new()
            .default_headers(headers)
            .timeout(Duration::from_secs(10))
            .use_rustls_tls()
            .build()?;

        let cache = Arc::new(Cache::new(client.clone(), config.clone()));

        let mut router = Router::new()
            .route("/aggregator/pool/pool.status", get(Self::pool_status))
            .route("/aggregator/users/{address}", get(Self::user_status))
            .route("/aggregator/users", get(Self::users));

        router = if let Some(token) = config.api_token() {
            router.layer(ValidateRequestHeaderLayer::bearer(token))
        } else {
            router
        }
        .route(
            "/aggregator/dashboard",
            Server::with_auth(config.clone(), get(Self::dashboard)),
        )
        .layer(Extension(cache))
        .layer(Extension(client))
        .layer(Extension(config.clone()));

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

    pub(crate) async fn dashboard(
        Extension(client): Extension<Client>,
        Extension(config): Extension<Arc<ServerConfig>>,
    ) -> ServerResult<Response> {
        let nodes = config.nodes();
        let admin_token = config.admin_token();
        let fetches = nodes.iter().map(|url| {
            let client = client.clone();
            async move {
                let result = async {
                    let mut request_builder = client
                        .get(url.join("/status")?)
                        .header("accept", "application/json");

                    if let Some(token) = admin_token {
                        request_builder = request_builder.bearer_auth(token);
                    }

                    let resp = request_builder.send().await?;

                    let status: Result<api::Status> =
                        serde_json::from_str(&resp.text().await?).map_err(|err| anyhow!(err));

                    status
                }
                .await;

                (url, result)
            }
        });

        let results: Vec<(&Url, Result<api::Status>)> = join_all(fetches).await;

        let mut checks = BTreeMap::new();

        for (url, result) in results {
            if let Ok(status) = result {
                checks.insert(url.host_str().unwrap_or("unknown").to_string(), status);
            }
        }

        Ok(DashboardHtml { statuses: checks }
            .page(config.domain())
            .into_response())
    }
}
