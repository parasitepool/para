use {
    super::*,
    backon::{ExponentialBuilder, Retryable},
    dashmap::DashMap,
};

async fn fetch_for<T, P>(client: Client, base: Url, path: String, parse: P) -> (Url, Result<T>)
where
    P: FnOnce(&str) -> Result<T> + Send + 'static,
{
    let result = async {
        let url = base.join(&path)?;
        let body = fetch(&client, url).await?;
        parse(&body)
    }
    .await;

    (base, result)
}

async fn fetch(client: &Client, url: Url) -> Result<String> {
    let started = Instant::now();

    let backoff = ExponentialBuilder::default()
        .with_min_delay(Duration::from_millis(300))
        .with_max_delay(Duration::from_secs(3))
        .with_jitter()
        .with_max_times(MAX_ATTEMPTS)
        .with_total_delay(Some(BUDGET));

    let fetch = || async {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= BUDGET {
            return Err(anyhow!("deadline exceeded"));
        }
        let remaining = BUDGET - elapsed;
        let this_try = remaining.min(TIMEOUT);

        let body = tokio::time::timeout(this_try, async {
            let response = client.get(url.clone()).send().await?;

            if let Err(status_err) = response.error_for_status_ref() {
                let text = response.text().await.unwrap_or_default();
                return Err(anyhow!("{status_err}: {text}").context(status_err));
            }

            let text = response.text().await?;
            Ok::<String, Error>(text)
        })
        .await
        .map_err(|_| anyhow::anyhow!("try timeout"))??;

        Ok::<String, Error>(body)
    };

    let body = fetch
        .retry(backoff)
        .sleep(tokio::time::sleep)
        .when(|err: &Error| {
            if let Some(err) = err.downcast_ref::<reqwest::Error>() {
                // Retry on network-level failures
                if err.is_timeout()
                    || err.is_connect()
                    || err.is_request()
                    || err.is_body()
                    || err.is_decode()
                {
                    return true;
                }

                // Retry on DNS resolution failures (shown as connection errors without status)
                if err.source().is_some_and(|e| {
                    e.to_string().contains("dns")
                        || e.to_string().contains("resolve")
                        || e.to_string().contains("getaddrinfo")
                }) {
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

    Ok(body)
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

        let fetches: FuturesUnordered<_> = self
            .config
            .nodes()
            .into_iter()
            .map(|base| {
                fetch_for(
                    self.client.clone(),
                    base,
                    "/pool/pool.status".into(),
                    ckpool::Status::from_str,
                )
            })
            .collect();

        let aggregated = fetches
            .fold(None, |acc, (base, res)| async move {
                match res {
                    Ok(status) => Some(match acc {
                        Some(a) => a + status,
                        None => status,
                    }),
                    Err(err) => {
                        let host = base.host_str().unwrap_or("unknown");
                        warn!("Failed to fetch pool status from {host} with: {err}");
                        acc
                    }
                }
            })
            .await;

        if aggregated.is_none() {
            error!("Failed to aggregate pool statistics");
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

        let fetches: FuturesUnordered<_> = self
            .config
            .nodes()
            .into_iter()
            .map(|base| {
                fetch_for(
                    self.client.clone(),
                    base,
                    format!("/users/{address}"),
                    |user| serde_json::from_str::<ckpool::User>(user).map_err(Into::into),
                )
            })
            .collect();

        let aggregated = fetches
            .fold(None, |acc, (_, res)| async move {
                match res {
                    Ok(status) => Some(match acc {
                        Some(a) => a + status,
                        None => status,
                    }),
                    Err(_) => acc,
                }
            })
            .await;

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
        let fetches: FuturesUnordered<_> = self
            .config
            .nodes()
            .into_iter()
            .map(|base| {
                fetch_for(self.client.clone(), base, "/users".to_string(), |s| {
                    serde_json::from_str::<Vec<String>>(s).map_err(Into::into)
                })
            })
            .collect();

        let set = fetches
            .fold(
                HashSet::<String>::new(),
                |mut acc, (base, res)| async move {
                    match res {
                        Ok(list) => acc.extend(list),
                        Err(err) => {
                            let host = base.host_str().unwrap_or("unknown");
                            warn!("Failed to fetch users from {host} with: {err}");
                        }
                    }
                    acc
                },
            )
            .await;

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
