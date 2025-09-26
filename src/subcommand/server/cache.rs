use {super::*, dashmap::DashMap};

struct Cached<T> {
    value: Option<T>,
    last_updated: Instant,
}

impl<T: Clone> Cached<T> {
    fn new(value: Option<T>) -> Self {
        Self {
            value,
            last_updated: Instant::now(),
        }
    }

    fn value(&self, ttl: Duration) -> Option<T> {
        if self.value.is_none() || self.last_updated.elapsed() >= ttl {
            None
        } else {
            self.value.clone()
        }
    }
}

pub(super) struct Cache {
    client: Client,
    config: Arc<ServerConfig>,
    ttl: Duration,
    pool_status: Mutex<Cached<ckpool::Status>>,
    users: DashMap<String, Arc<Mutex<Cached<ckpool::User>>>>,
}

impl Cache {
    pub(super) fn new(client: Client, config: Arc<ServerConfig>, ttl: Duration) -> Self {
        Self {
            client,
            config,
            ttl,
            pool_status: Mutex::new(Cached::new(None)),
            users: DashMap::new(),
        }
    }

    pub(super) async fn pool_status(&self) -> Result<ckpool::Status> {
        let mut cached = self.pool_status.lock().await;
        if let Some(status) = cached.value(self.ttl) {
            return Ok(status);
        }

        let nodes = self.config.nodes();
        let fetches = nodes.iter().map(|url| {
            let client = self.client.clone();
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
                Err(err) => warn!("Failed to fetch status from {url} with: {err}"),
            }
        }

        *cached = Cached::new(aggregated);

        aggregated.ok_or_else(|| anyhow!("Failed to aggregate statistics"))
    }

    pub(super) async fn user_status(&self, address: String) -> Result<Option<ckpool::User>> {
        let cell = self
            .users
            .entry(address.clone())
            .or_insert_with(|| Arc::new(Mutex::new(Cached::new(None))))
            .clone();

        let mut cached = cell.lock().await;
        if let Some(user) = cached.value(self.ttl) {
            return Ok(Some(user));
        }

        let nodes = self.config.nodes();
        let fetches = nodes.iter().map(|url| {
            let client = self.client.clone();
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

        *cached = Cached::new(aggregated.clone());

        Ok(aggregated)
    }
}
