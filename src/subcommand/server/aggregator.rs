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
    pub(crate) fn init(nodes: Vec<Url>) -> Result<Router> {
        let client = ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .use_rustls_tls()
            .build()?;

        let nodes = Arc::new(nodes);

        let router = Router::new()
            .route("/aggregator/pool/pool.status", get(Self::pool_status))
            .route("/aggregator/users/{address}", get(Self::user_status))
            .layer(Extension(client))
            .layer(Extension(nodes));

        Ok(router)
    }

    async fn pool_status(
        Extension(client): Extension<Client>,
        Extension(nodes): Extension<Arc<Vec<Url>>>,
    ) -> ServerResult<Response> {
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
        Extension(nodes): Extension<Arc<Vec<Url>>>,
    ) -> ServerResult<Response> {
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
}
