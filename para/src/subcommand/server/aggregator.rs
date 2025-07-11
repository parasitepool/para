use {super::*, futures::future::join_all, pool::Status, reqwest::Client};

mod pool;
mod user;

pub(crate) struct Aggregator;

impl Aggregator {
    pub(crate) fn init(nodes: Vec<Url>) -> Result<Router> {
        Ok(Router::new()
            .route("/aggregator/pool/pool.status", get(Self::pool_status))
            .layer(Extension(nodes)))
    }

    async fn pool_status(Extension(nodes): Extension<Vec<Url>>) -> ServerResult<Response> {
        let client = Client::new();
        let fetches = nodes.into_iter().map(|node_url| {
            let client = client.clone();
            async move {
                let mut url = node_url.clone();
                url.set_path("/pool/pool.status");
                let result = async {
                    let resp = client.get(url).send().await?;
                    let text = resp.text().await?;
                    Status::from_str(&text)
                }
                .await;
                (node_url, result)
            }
        });

        let results: Vec<(Url, Result<Status>)> = join_all(fetches).await;

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
}
