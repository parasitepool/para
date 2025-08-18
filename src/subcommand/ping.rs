use {
    super::*,
    std::net::SocketAddr,
    tokio::time::{Instant, sleep},
};

#[derive(Parser, Debug)]
#[command(about = "Ping a stratum mining server.")]
pub(crate) struct Ping {
    target: String,
    #[arg(long, help = "Stop after <COUNT> replies")]
    count: Option<u64>,
    #[arg(long, default_value = "5", help = "Fail after <TIMEOUT> seconds")]
    timeout: u64,
}

impl Ping {
    pub(crate) async fn run(&self) -> Result {
        let addr = self.resolve_target().await?;

        println!("SUBSCRIBE PING {} ({})", self.target, addr);

        let stats = Arc::new(PingStats::new());
        let sequence = AtomicU64::new(0);

        let mut reply_count = 0;
        let mut success = false;

        loop {
            tokio::select! {
                _ = ctrl_c() => break,
                _ = sleep(Duration::from_secs(1)) => {
                    let seq = sequence.fetch_add(1, Ordering::Relaxed);

                    match self.ping_once(addr, seq).await {
                        Ok((size, duration)) => {
                            success = true;
                            stats.record_success(duration);
                            println!("Response from {addr}: seq={seq} size={size} time={:.3}ms", duration.as_secs_f64() * 1000.0);
                        }
                        Err(e) => {
                            stats.record_failure();
                            println!("Request timeout for seq={seq} ({e})");
                        }
                    }
                    reply_count += 1;
                    if let Some(count) = self.count &&
                         count == reply_count {
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
        let host_port = if self.target.contains(':') {
            self.target.clone()
        } else {
            format!("{}:42069", self.target)
        };

        let addr = tokio::net::lookup_host(&host_port)
            .await?
            .next()
            .ok_or_else(|| anyhow::anyhow!("Failed to resolve hostname"))?;

        Ok(addr)
    }

    async fn ping_once(&self, addr: SocketAddr, sequence: u64) -> Result<(usize, Duration)> {
        let request = Message::Request {
            id: Id::Number(sequence),
            method: "mining.subscribe".into(),
            params: serde_json::to_value(stratum::Subscribe {
                user_agent: "user ParaPing/0.0.1".into(),
                extranonce1: None,
            })?,
        };

        let frame = serde_json::to_string(&request)? + "\n";

        let mut stream =
            tokio::time::timeout(Duration::from_secs(self.timeout), TcpStream::connect(addr))
                .await??;

        let start = Instant::now();

        stream.write_all(frame.as_bytes()).await?;

        let mut reader = BufReader::new(&mut stream);
        let mut response_line = String::new();
        let bytes_read = reader.read_line(&mut response_line).await?;

        let duration = start.elapsed();

        let _: serde_json::Value = serde_json::from_str(response_line.trim())?;

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

    fn record_success(&self, duration: Duration) {
        self.sent.fetch_add(1, Ordering::Relaxed);
        self.received.fetch_add(1, Ordering::Relaxed);

        let duration_ns = duration.as_nanos() as u64;
        self.total_time_ns.fetch_add(duration_ns, Ordering::Relaxed);

        self.min_time_ns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if duration_ns < current {
                    Some(duration_ns)
                } else {
                    None
                }
            })
            .ok();

        self.max_time_ns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if duration_ns > current {
                    Some(duration_ns)
                } else {
                    None
                }
            })
            .ok();
    }

    fn record_failure(&self) {
        self.sent.fetch_add(1, Ordering::Relaxed);
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

fn print_final_stats(target: &str, stats: &PingStats) {
    let (sent, received, loss_percent, min_ms, avg_ms, max_ms) = stats.get_stats();

    println!("\n--- {target} ping statistics ---");
    println!("{sent} packets transmitted, {received} received, {loss_percent:.1}% packet loss");

    if received > 0 {
        println!("round-trip min/avg/max = {min_ms:.3}/{avg_ms:.3}/{max_ms:.3} ms");
    }
}
