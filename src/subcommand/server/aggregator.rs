use super::*;

pub(crate) struct Aggregator;

impl Aggregator {
    pub(crate) fn init(state: ServerState) -> Result<Router> {
        let mut headers = header::HeaderMap::new();
        if let Some(token) = state.config.api_token(&state.settings) {
            headers.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }

        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_str("application/json")?,
        );

        let client = ClientBuilder::new()
            .default_headers(headers)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .pool_idle_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(3)
            .use_rustls_tls()
            .build()?;

        let cache = Arc::new(Cache::new(client.clone(), state.clone()));

        let mut router = Router::new()
            .route("/aggregator/pool/pool.status", get(Self::pool_status))
            .route("/aggregator/users/{address}", get(Self::user_status))
            .route("/aggregator/users", get(Self::users));

        router = if let Some(token) = state.config.api_token(&state.settings) {
            router.layer(bearer_auth(&token))
        } else {
            router
        }
        .route(
            "/aggregator/dashboard",
            Server::with_auth(&state.config, &state.settings, get(Self::dashboard)),
        )
        .layer(Extension(cache))
        .layer(Extension(client))
        .layer(Extension(state));

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
        Extension(state): Extension<ServerState>,
    ) -> ServerResult<Response> {
        let mut nodes = state.config.nodes(&state.settings);
        if let Some(sync_endpoint) = state.config.sync_endpoint(&state.settings) {
            nodes.push(Url::from_str(&sync_endpoint).map_err(|err| anyhow!(err))?);
        }
        let admin_token = state.config.admin_token(&state.settings);
        let fetches = nodes.iter().map(|url| {
            let client = client.clone();
            let admin_token = admin_token.clone();
            async move {
                let result = async {
                    let mut request_builder = client.get(url.join("/status")?);

                    if let Some(ref token) = admin_token {
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

        let results: Vec<(&Url, Result<api::Status>)> = futures::future::join_all(fetches).await;

        let mut checks = BTreeMap::new();

        for (url, result) in results {
            if let Ok(status) = result {
                checks.insert(url.host_str().unwrap_or("unknown").to_string(), status);
            }
        }

        Ok(DashboardHtml { statuses: checks }
            .page(state.config.domain(&state.settings))
            .into_response())
    }
}
