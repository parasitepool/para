use {super::*, dashmap::DashMap};

#[derive(Debug)]
struct Cached<T> {
    value: Option<T>,
    last_updated: Instant,
}

impl<T: Clone> Cached<T> {
    fn init(ttl: Duration) -> Self {
        Self {
            value: None,
            last_updated: Instant::now() - ttl,
        }
    }

    fn new(value: Option<T>) -> Self {
        Self {
            value,
            last_updated: Instant::now(),
        }
    }

    fn is_fresh(&self, ttl: Duration) -> bool {
        self.last_updated.elapsed() < ttl
    }

    fn value(&self) -> Option<T> {
        self.value.clone()
    }
}

#[derive(Debug)]
pub(super) struct Cache {
    client: Client,
    config: Arc<ServerConfig>,
    pool_status: Mutex<Cached<ckpool::Status>>,
    user_statuses: DashMap<String, Arc<Mutex<Cached<ckpool::User>>>>,
    users: Mutex<Cached<Vec<String>>>,
}

impl Cache {
    pub(super) fn new(client: Client, config: Arc<ServerConfig>) -> Self {
        Self {
            client,
            config: config.clone(),
            pool_status: Mutex::new(Cached::init(config.ttl())),
            user_statuses: DashMap::new(),
            users: Mutex::new(Cached::init(config.ttl())),
        }
    }

    pub(super) async fn pool_status(&self) -> Result<Option<ckpool::Status>> {
        let mut cached = self.pool_status.lock().await;
        if cached.is_fresh(self.config.ttl()) {
            return Ok(cached.value());
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

        if aggregated.is_none() {
            error!("Failed aggregate pool statistics");
        }

        *cached = Cached::new(aggregated);

        Ok(aggregated)
    }

    pub(super) async fn user_status(&self, address: String) -> Result<Option<ckpool::User>> {
        let cell = self
            .user_statuses
            .entry(address.clone())
            .or_insert_with(|| Arc::new(Mutex::new(Cached::init(self.config.ttl()))))
            .clone();

        let mut cached = cell.lock().await;
        if cached.is_fresh(self.config.ttl()) {
            return Ok(cached.value());
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

        if aggregated.is_none() {
            error!("Failed to find user {address} on any node");
        }

        *cached = Cached::new(aggregated.clone());

        Ok(aggregated)
    }

    pub(super) async fn users(&self) -> Result<Option<Vec<String>>> {
        let mut cached = self.users.lock().await;
        if cached.is_fresh(self.config.ttl()) {
            return Ok(cached.value());
        }

        let nodes = self.config.nodes();
        let fetches = nodes.iter().map(|url| {
            let client = self.client.clone();
            async move {
                let result = async {
                    let resp = client.get(url.join("/users")?).send().await?;
                    serde_json::from_str::<Vec<String>>(&resp.text().await?).map_err(Into::into)
                }
                .await;
                (url, result)
            }
        });

        let results: Vec<(&Url, Result<Vec<String>>)> = join_all(fetches).await;

        let mut set = HashSet::new();
        for (url, result) in results {
            match result {
                Ok(users) => set.extend(users),
                Err(err) => warn!("Failed to fetch status from {url} with: {err}"),
            }
        }

        let aggregated = if set.is_empty() {
            error!("Failed aggregate users");
            None
        } else {
            Some(set.into_iter().collect::<Vec<String>>())
        };

        *cached = Cached::new(aggregated.clone());

        Ok(aggregated)
    }
}
