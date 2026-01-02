use super::*;

#[derive(Parser, Debug)]
pub(crate) struct Ping {
    #[arg(help = "Stratum <HOST:PORT>.")]
    stratum_endpoint: String,
    #[arg(long, help = "Stop after <COUNT> replies.")]
    count: Option<u64>,
    #[arg(long, default_value = "5", help = "Fail after <TIMEOUT> seconds.")]
    timeout: u64,
    #[arg(long, help = "Stratum <USERNAME>.")]
    username: Option<Username>,
    #[arg(long, help = "Stratum <PASSWORD>.")]
    password: Option<String>,
}

impl Ping {
    pub(crate) async fn run(&self, cancel_token: CancellationToken) -> Result {
        let addr = resolve_stratum_endpoint(&self.stratum_endpoint)
            .await
            .with_context(|| {
                format!(
                    "failed to resolve stratum endpoint `{}`",
                    self.stratum_endpoint
                )
            })?;

        let ping_type = PingType::new(self.username.clone(), self.password.as_deref());

        println!("{} {} ({})", ping_type, self.stratum_endpoint, addr);

        let stats = Arc::new(PingStats::new());
        let sequence = AtomicU64::new(0);

        let mut reply_count = 0;
        let mut success = false;

        let mut interval = interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        interval.tick().await;

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => break,
                _ = interval.tick() => {
                    let seq = sequence.fetch_add(1, Ordering::Relaxed);
                    stats.record_attempt();

                    tokio::select! {
                        _ = cancel_token.cancelled() => {
                            println!("Ping cancelled for seq={seq}");
                            break;
                        }
                        result = self.ping_once(addr, &ping_type) => {
                            match result {
                                Ok((duration, size)) => {
                                    success = true;
                                    stats.record_success(duration);
                                    println!("Response from {addr}: seq={seq} size={size} time={:.3}ms", duration.as_secs_f64() * 1000.0);
                                }
                                Err(e) => {
                                    println!("Request timeout for seq={seq} ({e})");
                                }
                            }
                        }
                    }

                    reply_count += 1;
                    if let Some(count) = self.count && count == reply_count {
                        break;
                    }
                }
            }
        }

        print_final_stats(&self.stratum_endpoint, &stats);

        if success {
            Ok(())
        } else {
            Err(anyhow!("Ping timed out"))
        }
    }

    async fn ping_once(&self, addr: SocketAddr, ping_type: &PingType) -> Result<(Duration, usize)> {
        match ping_type {
            PingType::Subscribe => {
                let config = stratum::ClientConfig {
                    address: addr.to_string(),
                    username: "".into(),
                    user_agent: USER_AGENT.into(),
                    password: None,
                    timeout: Duration::from_secs(self.timeout),
                };

                let client = stratum::Client::new(config);
                client.connect().await?;

                let (_, duration, size) = client.subscribe().await?;

                client.disconnect().await?;

                Ok((duration, size))
            }
            PingType::Authorized { username, password } => {
                let config = stratum::ClientConfig {
                    address: addr.to_string(),
                    username: username.clone(),
                    user_agent: USER_AGENT.into(),
                    password: Some(password.clone()),
                    timeout: Duration::from_secs(self.timeout),
                };

                let client = stratum::Client::new(config);
                let mut events = client.connect().await?;

                client.subscribe().await?;
                let (duration, size) = client.authorize().await?;

                let instant = Instant::now();

                loop {
                    match events.recv().await {
                        Ok(stratum::Event::Notify(_)) => {
                            break;
                        }
                        Ok(stratum::Event::Disconnected) => {
                            return Err(anyhow!("Disconnected before notify"));
                        }
                        Err(e) => {
                            return Err(anyhow!("Stratum event error: {}", e));
                        }
                        _ => continue,
                    }
                }

                let duration = duration + instant.elapsed();

                client.disconnect().await?;

                Ok((duration, size))
            }
        }
    }
}

#[derive(Debug, Clone)]
enum PingType {
    Subscribe,
    Authorized {
        username: Username,
        password: String,
    },
}

impl PingType {
    fn new(username: Option<Username>, password: Option<&str>) -> Self {
        match username {
            Some(user) => Self::Authorized {
                username: user,
                password: password.unwrap_or("x").to_string(),
            },
            None => Self::Subscribe,
        }
    }
}

impl fmt::Display for PingType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PingType::Subscribe => write!(f, "SUBSCRIBE PING"),
            PingType::Authorized { .. } => write!(f, "AUTHORIZED PING"),
        }
    }
}

struct PingStats {
    sent: AtomicU64,
    received: AtomicU64,
    total_time_ns: AtomicU64,
    min_time_ns: AtomicU64,
    max_time_ns: AtomicU64,
}

impl PingStats {
    fn new() -> Self {
        Self {
            sent: AtomicU64::new(0),
            received: AtomicU64::new(0),
            total_time_ns: AtomicU64::new(0),
            min_time_ns: AtomicU64::new(u64::MAX),
            max_time_ns: AtomicU64::new(0),
        }
    }

    fn record_attempt(&self) {
        self.sent.fetch_add(1, Ordering::Relaxed);
    }

    fn record_success(&self, duration: Duration) {
        self.received.fetch_add(1, Ordering::Relaxed);

        let duration_ns = duration.as_nanos() as u64;
        self.total_time_ns.fetch_add(duration_ns, Ordering::Relaxed);

        let mut current = self.min_time_ns.load(Ordering::Relaxed);
        while duration_ns < current
            && self
                .min_time_ns
                .compare_exchange(current, duration_ns, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            current = self.min_time_ns.load(Ordering::Relaxed);
        }

        let mut current = self.max_time_ns.load(Ordering::Relaxed);
        while duration_ns > current
            && self
                .max_time_ns
                .compare_exchange(current, duration_ns, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            current = self.max_time_ns.load(Ordering::Relaxed);
        }
    }

    fn get_stats(&self) -> (u64, u64, f64, f64, f64, f64) {
        let sent = self.sent.load(Ordering::Relaxed);
        let received = self.received.load(Ordering::Relaxed);
        let total_time_ns = self.total_time_ns.load(Ordering::Relaxed);
        let min_time_ns = self.min_time_ns.load(Ordering::Relaxed);
        let max_time_ns = self.max_time_ns.load(Ordering::Relaxed);

        let loss_percent = if sent > 0 {
            100.0 * (sent - received) as f64 / sent as f64
        } else {
            0.0
        };

        let avg_ms = if received > 0 {
            (total_time_ns as f64 / received as f64) / 1_000_000.0
        } else {
            0.0
        };

        let min_ms = if min_time_ns != u64::MAX {
            min_time_ns as f64 / 1_000_000.0
        } else {
            0.0
        };

        let max_ms = max_time_ns as f64 / 1_000_000.0;

        (sent, received, loss_percent, min_ms, avg_ms, max_ms)
    }
}

impl fmt::Display for PingStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (sent, received, loss_percent, min_ms, avg_ms, max_ms) = self.get_stats();

        writeln!(
            f,
            "{sent} packets transmitted, {received} received, {loss_percent:.1}% packet loss"
        )?;

        if received > 0 {
            writeln!(
                f,
                "round-trip min/avg/max = {min_ms:.3}/{avg_ms:.3}/{max_ms:.3} ms"
            )?;
        }

        Ok(())
    }
}

fn print_final_stats(stratum_endpoint: &str, stats: &PingStats) {
    println!("\n--- {stratum_endpoint} ping statistics ---");
    print!("{stats}");
}
