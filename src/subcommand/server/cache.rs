use {
    super::*,
    backon::{ExponentialBuilder, Retryable},
    dashmap::DashMap,
};

pub async fn fetch_and_parse<T, F>(
    client: &Client,
    url: Url,
    budget: Duration,
    timeout: Duration,
    max_attempts: usize,
    parse: F,
) -> Result<T>
where
    F: FnOnce(&str) -> Result<T>,
{
    let started = Instant::now();

    let backoff = ExponentialBuilder::default()
        .with_min_delay(Duration::from_millis(111))
        .with_max_delay(Duration::from_millis(999))
        .with_jitter()
        .with_max_times(max_attempts);

    let op = || async {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= budget {
            return Err(anyhow!("deadline exceeded"));
        }
        let remaining = budget - elapsed;
        let this_try = remaining.min(timeout);

        let request = client.get(url.clone());

        let response = match tokio::time::timeout(this_try, request.send()).await {
            Err(_) => return Err(anyhow::anyhow!("try timeout")),
            Ok(Err(err)) => return Err(err.into()),
            Ok(Ok(response)) => response,
        };

        dbg!(&response);

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("{}: {}", status, body));
        }

        Ok::<String, Error>(response.text().await?)
    };

    let body = op
        .retry(backoff)
        .sleep(tokio::time::sleep)
        .when(|err: &Error| {
            if let Some(err) = err.downcast_ref::<reqwest::Error>() {
                if err.is_timeout() || err.is_connect() || err.is_request() || err.is_body() {
                    return true;
                }

                if let Some(s) = err.status() {
                    return s == StatusCode::TOO_MANY_REQUESTS || s.is_server_error();
                }

                return false;
            }

            let error = err.to_string();
            if error.contains("try timeout") {
                return true;
            }

            if error.contains("deadline exceeded") {
                return false;
            }

            false
        })
        .notify(|err: &Error, duration: Duration| {
            tracing::debug!(?err, ?duration, "retrying after backoff");
        })
        .await?;

    parse(&body)
}

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
                let result = fetch_and_parse(
                    &client,
                    url.join("/pool/pool.status").unwrap(), //TODO
                    Duration::from_secs(5),
                    Duration::from_secs(2),
                    3,
                    ckpool::Status::from_str,
                )
                .await;
                dbg!(&result);
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
