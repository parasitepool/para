use {
    super::*,
    futures::future::join_all,
    pool::Status,
    reqwest::{Client, ClientBuilder},
};

mod pool;
mod user;

pub(crate) struct Aggregator;

impl Aggregator {
    pub(crate) fn init(config: Arc<Config>) -> Result<Router> {
        let client = ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .use_rustls_tls()
            .build()?;

        let router = Router::new()
            .route("/aggregator/pool/pool.status", get(Self::pool_status))
            .route("/aggregator/users/{address}", get(Self::user_status))
            .route("/aggregator/healthcheck", get(Self::healthcheck))
            .layer(Extension(client))
            .layer(Extension(config));

        Ok(router)
    }

    async fn pool_status(
        Extension(client): Extension<Client>,
        Extension(config): Extension<Arc<Config>>,
    ) -> ServerResult<Response> {
        let nodes = config.nodes();
        let fetches = nodes.iter().map(|url| {
            let client = client.clone();
            async move {
                let result = async {
                    let resp = client.get(url.join("/pool/pool.status")?).send().await?;
                    Status::from_str(&resp.text().await?)
                }
                .await;

                (url, result)
            }
        });

        let results: Vec<(&Url, Result<Status>)> = join_all(fetches).await;

        let mut aggregated: Option<Status> = None;
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
        Extension(config): Extension<Arc<Config>>,
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

                    serde_json::from_str::<user::User>(&resp.text().await?).map_err(Into::into)
                }
                .await;

                (url, result)
            }
        });

        let results: Vec<(&Url, Result<user::User>)> = join_all(fetches).await;

        let mut aggregated: Option<user::User> = None;
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

    pub(crate) async fn healthcheck(
        Extension(client): Extension<Client>,
        Extension(config): Extension<Arc<Config>>,
        AcceptJson(accept_json): AcceptJson,
    ) -> ServerResult<Response> {
        let nodes = config.nodes();
        let credentials = config.credentials();
        let fetches = nodes.iter().map(|url| {
            let client = client.clone();
            async move {
                let result = async {
                    let mut request_builder = client
                        .get(url.join("/healthcheck")?)
                        .header("accept", "application/json");

                    if let Some((username, password)) = credentials {
                        request_builder = request_builder.basic_auth(username, Some(password));
                    }

                    let resp = request_builder.send().await?;

                    let healthcheck: Result<HealthcheckHtml> =
                        serde_json::from_str(&resp.text().await?).map_err(|err| anyhow!(err));

                    healthcheck
                }
                .await;

                (url, result)
            }
        });

        let results: Vec<(&Url, Result<HealthcheckHtml>)> = join_all(fetches).await;

        let mut checks = Vec::new();

        for (_, result) in results {
            if let Ok(healthcheck) = result {
                checks.push(healthcheck)
            }
        }

        Ok(HealthcheckaggHtml { checks }
            .page(config.domain())
            .into_response())

        //  Ok(if accept_json {
        //      Json(healthcheck).into_response()
        //  } else {
        //      healthcheck.page(config.domain()).into_response()
        //  })
    }
}
