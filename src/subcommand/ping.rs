use super::*;

#[derive(Parser, Debug)]
#[command(about = "Ping a stratum mining server.")]
pub(crate) struct Ping {
    target: String,
    #[arg(long, help = "Stop after <COUNT> replies")]
    count: Option<u64>,
    #[arg(long, default_value = "5", help = "Fail after <TIMEOUT> seconds")]
    timeout: u64,
    #[arg(long, help = "Stratum <USERNAME>")]
    username: Option<String>,
    #[arg(long, help = "Stratum <PASSWORD>")]
    password: Option<String>,
}

impl Ping {
    pub(crate) async fn run(&self) -> Result {
        let addr = self.resolve_target().await?;

        let ping_type = PingType::new(self.username.as_deref(), self.password.as_deref());

        println!("{} {} ({})", ping_type, self.target, addr);

        let stats = Arc::new(PingStats::new());
        let sequence = AtomicU64::new(0);

        let mut reply_count = 0;
        let mut success = false;

        loop {
            tokio::select! {
                _ = ctrl_c() => break,
                _ = sleep(Duration::from_secs(1)) => {
                    let seq = sequence.fetch_add(1, Ordering::Relaxed);

                    stats.record_attempt();

                    match self.ping_once(addr, seq, &ping_type).await {
                        Ok((size, duration)) => {
                            success = true;
                            stats.record_success(duration);
                            println!("Response from {addr}: seq={seq} size={size} time={:.3}ms", duration.as_secs_f64() * 1000.0);
                        }
                        Err(e) => {
                            println!("Request timeout for seq={seq} ({e})");
                        }
                    }

                    reply_count += 1;
                    if let Some(count) = self.count && count == reply_count {
                        break;
                    }
                }
            }
        }

        print_final_stats(&self.target, &stats);

        if success {
            Ok(())
        } else {
            Err(anyhow!("Ping timed out"))
        }
    }

    async fn resolve_target(&self) -> Result<SocketAddr> {
        let target = if self.target.contains(':') {
            self.target.clone()
        } else {
            format!("{}:42069", self.target)
        };

        let addr = tokio::net::lookup_host(&target)
            .await?
            .next()
            .with_context(|| "Failed to resolve hostname")?;

        Ok(addr)
    }

    async fn ping_once(
        &self,
        addr: SocketAddr,
        sequence: u64,
        ping_type: &PingType,
    ) -> Result<(usize, Duration)> {
        let mut stream =
            tokio::time::timeout(Duration::from_secs(self.timeout), TcpStream::connect(addr))
                .await??;

        let mut reader = BufReader::new(&mut stream);

        match ping_type {
            PingType::Subscribe => self.subscribe_ping(&mut reader, sequence).await,
            PingType::Authorized { username, password } => {
                self.authenticated_ping(&mut reader, sequence, username, password)
                    .await
            }
        }
    }

    async fn subscribe_ping(
        &self,
        reader: &mut BufReader<&mut TcpStream>,
        sequence: u64,
    ) -> Result<(usize, Duration)> {
        let request = stratum::Message::Request {
            id: stratum::Id::Number(sequence),
            method: "mining.subscribe".into(),
            params: serde_json::to_value(stratum::Subscribe {
                user_agent: USER_AGENT.into(),
                extranonce1: None,
            })?,
        };

        let frame = serde_json::to_string(&request)? + "\n";

        let start = Instant::now();

        reader.get_mut().write_all(frame.as_bytes()).await?;

        let mut response_line = String::new();
        let bytes_read = tokio::time::timeout(
            Duration::from_secs(self.timeout),
            reader.read_line(&mut response_line),
        )
        .await??;

        let response: stratum::Message = serde_json::from_str(response_line.trim())
            .with_context(|| format!("Invalid JSON in subscribe response: {response_line:?}"))?;

        match response {
            stratum::Message::Response {
                error: Some(error), ..
            } => {
                bail!("Server error in subscribe: {}", error);
            }
            stratum::Message::Response { .. } => {}
            _ => {
                bail!("Expected response, got: {:?}", response);
            }
        }

        let duration = start.elapsed();

        Ok((bytes_read, duration))
    }

    async fn authenticated_ping(
        &self,
        reader: &mut BufReader<&mut TcpStream>,
        sequence: u64,
        username: &str,
        password: &str,
    ) -> Result<(usize, Duration)> {
        self.subscribe_ping(reader, sequence).await?;

        let auth_start = Instant::now();

        let authorize_request = stratum::Message::Request {
            id: stratum::Id::Number(sequence + 1),
            method: "mining.authorize".into(),
            params: serde_json::to_value((username, password))?,
        };

        let frame = serde_json::to_string(&authorize_request)? + "\n";
        reader.get_mut().write_all(frame.as_bytes()).await?;

        let mut auth_completed = false;
        let mut total_bytes = 0;
        let auth_deadline = tokio::time::Instant::now() + Duration::from_secs(self.timeout);

        while tokio::time::Instant::now() < auth_deadline && !auth_completed {
            match tokio::time::timeout(Duration::from_millis(500), self.read_next_message(reader))
                .await
            {
                Ok(Ok((bytes, message))) => {
                    total_bytes += bytes;

                    match message {
                        stratum::Message::Response {
                            id, result, error, ..
                        } => {
                            if let stratum::Id::Number(response_id) = id
                                && response_id == sequence + 1
                            {
                                match (result, error) {
                                    (_, Some(error)) => {
                                        debug!("Authentication error: {}", error);
                                    }
                                    (Some(result), None) => {
                                        if let Some(result_bool) = result.as_bool() {
                                            if result_bool {
                                                debug!("Authentication successful");
                                            } else {
                                                debug!("Authentication rejected by server");
                                            }
                                        } else {
                                            debug!(
                                                "Authentication response received (non-boolean result)"
                                            );
                                        }
                                    }
                                    (None, None) => {
                                        debug!("Authentication response received");
                                    }
                                }
                                auth_completed = true;
                            }
                        }
                        stratum::Message::Notification { method, params } => {
                            // if mining notification record ping here
                        }
                        _ => { // do nothing 
                        }
                    }
                }
                Ok(Err(_)) => {
                    break;
                }
                Err(_) => {
                    continue;
                }
            }
        }

        if !auth_completed {
            println!("Authentication response not received within timeout");
        }

        let auth_duration = auth_start.elapsed();
        Ok((total_bytes, auth_duration))
    }

    async fn read_next_message(
        &self,
        reader: &mut BufReader<&mut TcpStream>,
    ) -> Result<(usize, stratum::Message)> {
        let mut response_line = String::new();
        let bytes_read = reader.read_line(&mut response_line).await?;

        let message: stratum::Message = serde_json::from_str(response_line.trim())
            .with_context(|| format!("Invalid JSON in server message: {response_line:?}"))?;

        Ok((bytes_read, message))
    }
}

#[derive(Debug, Clone)]
enum PingType {
    Subscribe,
    Authorized { username: String, password: String },
}

impl PingType {
    fn new(username: Option<&str>, password: Option<&str>) -> Self {
        match username {
            Some(user) => Self::Authorized {
                username: user.to_string(),
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

fn print_final_stats(target: &str, stats: &PingStats) {
    println!("\n--- {target} ping statistics ---");
    print!("{stats}");
}
