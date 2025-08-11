use {
    super::*,
    anyhow::{Context, bail},
    serde::Deserialize,
    std::net::SocketAddr,
    tokio::time::sleep,
};

#[derive(Deserialize)]
struct BasicResponse {
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct StratumFrame {
    method: Option<String>,
    params: Option<serde_json::Value>,
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
}

#[derive(Parser, Debug)]
#[command(about = "Ping a stratum mining server.")]
pub(crate) struct Ping {
    target: String,
    #[arg(long, help = "Stratum <USERNAME>")]
    username: Option<String>,
    #[arg(long, help = "Stratum <PASSWORD>")]
    password: Option<String>,
    #[arg(
        long,
        help = "Show additional server messages after ping",
        default_value = "false"
    )]
    show_messages: bool,
    #[arg(
        long,
        help = "Timeout for reading additional messages (seconds)",
        default_value = "5"
    )]
    message_timeout: u64,
}

impl Ping {
    pub(crate) async fn run(&self) -> Result {
        let addr = self.resolve_target().await?;

        let ping_type = if self.username.is_some() {
            "AUTHORIZED PING"
        } else {
            "SUBSCRIBE PING"
        };

        println!("{} {} ({})", ping_type, self.target, addr);

        let stats = Arc::new(PingStats::new());
        let sequence = AtomicU64::new(0);

        loop {
            tokio::select! {
                _ = ctrl_c() => break,
                _ = sleep(Duration::from_secs(1)) => {
                    let seq = sequence.fetch_add(1, Ordering::Relaxed);

                    stats.record_attempt();

                    match self.ping_once(addr, seq).await {
                        Ok((size, duration)) => {
                            stats.record_success(duration);
                            println!("Response from {addr}: seq={seq} size={size} time={:.3}ms", duration.as_secs_f64() * 1000.0);
                        }
                        Err(e) => {
                            println!("Request timeout for seq={seq} ({e})");
                        }
                    }
                }
            }
        }

        print_final_stats(&self.target, &stats);
        Ok(())
    }

    async fn resolve_target(&self) -> Result<SocketAddr> {
        let addr = if self.target.contains(':') {
            tokio::net::lookup_host(&self.target)
                .await?
                .next()
                .with_context(|| "Failed to resolve hostname")?
        } else {
            tokio::net::lookup_host(&format!("{}:42069", self.target))
                .await?
                .next()
                .with_context(|| "Failed to resolve hostname")?
        };

        Ok(addr)
    }

    async fn ping_once(&self, addr: SocketAddr, sequence: u64) -> Result<(usize, Duration)> {
        let mut stream =
            tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr)).await??;

        let mut reader = BufReader::new(&mut stream);

        if let Some(ref username) = self.username {
            self.authenticated_ping(&mut reader, sequence, username)
                .await
        } else {
            let start = Instant::now();
            let bytes_read = self.send_subscribe(&mut reader, sequence).await?;
            let duration = start.elapsed();
            Ok((bytes_read, duration))
        }
    }

    async fn send_subscribe(
        &self,
        reader: &mut BufReader<&mut TcpStream>,
        sequence: u64,
    ) -> Result<usize> {
        let request = Message::Request {
            id: Id::Number(sequence),
            method: "mining.subscribe".into(),
            params: serde_json::to_value(stratum::Subscribe {
                user_agent: "user ParaPing/0.0.1".into(),
                extranonce1: None,
            })?,
        };

        let frame = serde_json::to_string(&request)? + "\n";
        reader.get_mut().write_all(frame.as_bytes()).await?;

        let mut response_line = String::new();
        let bytes_read = reader.read_line(&mut response_line).await?;

        let response: BasicResponse = serde_json::from_str(response_line.trim())
            .with_context(|| format!("Invalid JSON in subscribe response: {response_line:?}"))?;

        if let Some(error) = response.error {
            bail!("Server error in subscribe: {}", error);
        }

        Ok(bytes_read)
    }

    async fn authenticated_ping(
        &self,
        reader: &mut BufReader<&mut TcpStream>,
        sequence: u64,
        username: &str,
    ) -> Result<(usize, Duration)> {
        self.send_subscribe(reader, sequence).await?;

        let password = self.password.as_deref().unwrap_or("x");
        let authorize_request = Message::Request {
            id: Id::Number(sequence + 1),
            method: "mining.authorize".into(),
            params: serde_json::to_value((username, password))?,
        };

        let frame = serde_json::to_string(&authorize_request)? + "\n";
        reader.get_mut().write_all(frame.as_bytes()).await?;

        let mut response_line = String::new();
        let _bytes_read = reader.read_line(&mut response_line).await?;

        let authorize_response: BasicResponse = serde_json::from_str(response_line.trim())
            .with_context(|| format!("Invalid JSON in authorize response: {response_line:?}"))?;

        if let Some(error) = authorize_response.error {
            bail!("Server error in authorize: {}", error);
        }

        if let Some(result) = authorize_response.result {
            if let Some(result_bool) = result.as_bool() {
                if !result_bool {
                    bail!("Authorization failed");
                }
            }
        }

        let (first_message_size, first_message_duration) =
            self.read_and_display_message(reader, true).await?;

        if self.show_messages {
            let timeout = self.message_timeout;
            println!("  Reading additional server messages for {timeout}s...");
            let message_deadline =
                tokio::time::Instant::now() + Duration::from_secs(self.message_timeout);

            while tokio::time::Instant::now() < message_deadline {
                match tokio::time::timeout(
                    Duration::from_millis(100),
                    self.read_and_display_message(reader, false),
                )
                .await
                {
                    Ok(Ok((_, _))) => {
                        continue;
                    }
                    Ok(Err(_)) => {
                        break;
                    }
                    Err(_) => {
                        continue;
                    }
                }
            }
        }

        Ok((first_message_size, first_message_duration))
    }

    async fn read_and_display_message(
        &self,
        reader: &mut BufReader<&mut TcpStream>,
        is_first_message: bool,
    ) -> Result<(usize, Duration)> {
        let start = Instant::now();
        let mut response_line = String::new();
        let bytes_read = tokio::time::timeout(
            Duration::from_secs(if is_first_message { 30 } else { 1 }),
            reader.read_line(&mut response_line),
        )
        .await??;

        let duration = start.elapsed();

        let frame: StratumFrame = serde_json::from_str(response_line.trim())
            .with_context(|| format!("Invalid JSON in server message: {response_line:?}"))?;

        if let Some(error) = &frame.error {
            bail!("Server error in message: {}", error);
        }

        let message_type = if let Some(ref method) = frame.method {
            format!("method={method}")
        } else if frame.result.is_some() {
            "response".to_string()
        } else {
            "unknown".to_string()
        };

        let prefix = if is_first_message {
            "  First message"
        } else {
            "  Additional message"
        };
        println!(
            "{prefix}: {message_type} size={bytes_read} time={:.3}ms",
            duration.as_secs_f64() * 1000.0
        );

        if let Some(ref method) = frame.method {
            match method.as_str() {
                "mining.notify" => {
                    if let Some(params) = &frame.params {
                        if let Some(params_array) = params.as_array() {
                            if let Some(job_id) = params_array.first().and_then(|v| v.as_str()) {
                                println!("    └─ Job ID: {job_id}");
                            }
                        }
                    }
                }
                "mining.set_difficulty" => {
                    if let Some(params) = &frame.params {
                        if let Some(params_array) = params.as_array() {
                            if let Some(difficulty) = params_array.first() {
                                println!("    └─ Difficulty: {difficulty}");
                            }
                        }
                    }
                }
                "mining.set_extranonce" => {
                    println!("    └─ Extra nonce update received");
                }
                _ => {
                    println!("    └─ Method: {method}");
                }
            }
        }

        Ok((bytes_read, duration))
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
