use {
    super::*,
    crate::ckpool,
    futures::future::join_all,
    reqwest::{Client, ClientBuilder},
};

pub(crate) struct Aggregator;

impl Aggregator {
    pub(crate) fn init(config: Arc<ServerConfig>) -> Result<Router> {
        let client = ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .use_rustls_tls()
            .build()?;

        let router = Router::new()
            .route("/aggregator/pool/pool.status", get(Self::pool_status))
            .route("/aggregator/users/{address}", get(Self::user_status))
            .route(
                "/aggregator/dashboard",
                Server::with_auth(config.clone(), get(Self::dashboard)),
            )
            .layer(Extension(client))
            .layer(Extension(config));

        Ok(router)
    }

    async fn pool_status(
        Extension(client): Extension<Client>,
        Extension(config): Extension<Arc<ServerConfig>>,
    ) -> ServerResult<Response> {
        let nodes = config.nodes();
        let fetches = nodes.iter().map(|url| {
            let client = client.clone();
            async move {
                let result = async {
                    let resp = client.get(url.join("/pool/pool.status")?).send().await?;
                    ckpool::Status::from_str(&resp.text().await?)
                }
                .await;

                (url, result)
            }
        });

        let results: Vec<(&Url, Result<ckpool::Status>)> = join_all(fetches).await;

        let mut aggregated: Option<ckpool::Status> = None;
        for (url, result) in results {
            match result {
                Ok(status) => {
                    aggregated = Some(if let Some(agg) = aggregated {
                        agg + status
                    } else {
                        status
                    });
                }
                Err(err) => {
                    warn!("Failed to fetch status from {url} with: {err}");
                }
            }
        }

        let aggregated = aggregated.ok_or_else(|| anyhow!("Failed to aggregate statistics"))?;

        Ok(aggregated.to_string().into_response())
    }

    async fn user_status(
        Path(address): Path<String>,
        Extension(client): Extension<Client>,
        Extension(config): Extension<Arc<ServerConfig>>,
    ) -> ServerResult<Response> {
        let nodes = config.nodes();
        let fetches = nodes.iter().map(|url| {
            let client = client.clone();
            let address = address.clone();
            async move {
                let result = async {
                    let resp = client
                        .get(url.join(&format!("/users/{address}"))?)
                        .send()
                        .await?;

                    serde_json::from_str::<ckpool::User>(&resp.text().await?).map_err(Into::into)
                }
                .await;

                (url, result)
            }
        });

        let results: Vec<(&Url, Result<ckpool::User>)> = join_all(fetches).await;

        let mut aggregated: Option<ckpool::User> = None;
        for (_, result) in results {
            if let Ok(user) = result {
                aggregated = Some(if let Some(agg) = aggregated {
                    agg + user
                } else {
                    user
                });
            }
        }

        let aggregated =
            aggregated.ok_or_else(|| anyhow!("User {address} not found on any node"))?;

        Ok(Json(aggregated).into_response())
    }

    pub(crate) async fn dashboard(
        Extension(client): Extension<Client>,
        Extension(config): Extension<Arc<ServerConfig>>,
    ) -> ServerResult<Response> {
        let nodes = config.nodes();
        let credentials = config.credentials();
        let fetches = nodes.iter().map(|url| {
            let client = client.clone();
            async move {
                let result = async {
                    let mut request_builder = client
                        .get(url.join("/healthcheck")?) // TODO: change to status
                        .header("accept", "application/json");

                    if let Some((username, password)) = credentials {
                        request_builder = request_builder.basic_auth(username, Some(password));
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
