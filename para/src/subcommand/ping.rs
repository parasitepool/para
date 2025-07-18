use {
    super::*,
    clap::Parser,
    serde::Serialize,
    serde_json::json,
    std::{
        net::{SocketAddr, ToSocketAddrs},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    },
    tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpStream,
        signal,
        time::sleep,
    },
};

#[derive(Parser, Debug)]
#[command(about = "Ping stratum mining servers")]
pub struct Ping {
    pub target: String,
}

#[derive(Debug, Serialize)]
struct StratumRequest {
    id: u64,
    method: String,
    params: Vec<serde_json::Value>,
}

impl Ping {
    pub async fn run(&self, _handle: Handle) -> Result {
        let addr = self.resolve_target().await?;

        println!("PING {} ({})", self.target, addr);

        let stats = Arc::new(PingStats::new());
        let should_stop = Arc::new(AtomicBool::new(false));
        let sequence = Arc::new(AtomicU64::new(0));

        // May try to incorporate `tokio::select` instead
        let should_stop_clone = Arc::clone(&should_stop);
        let stats_clone = Arc::clone(&stats);
        let target_clone = self.target.clone();

        tokio::spawn(async move {
            signal::ctrl_c().await.ok();
            should_stop_clone.store(true, Ordering::Relaxed);
            print_final_stats(&target_clone, &stats_clone);
            std::process::exit(0);
        });

        loop {
            if should_stop.load(Ordering::Relaxed) {
                break;
            }

            let seq = sequence.fetch_add(1, Ordering::Relaxed);
            let start = Instant::now();

            // Considering parallelizing with `join_all`
            match self.ping_once(addr, seq).await {
                Ok(response_size) => {
                    let duration = start.elapsed();
                    stats.record_success(duration);
                    println!(
                        "Response from {}: seq={} time={:.3}ms size={}",
                        addr,
                        seq,
                        duration.as_secs_f64() * 1000.0,
                        response_size
                    );
                }
                Err(e) => {
                    stats.record_failure();
                    println!("Request timeout for seq={} ({})", seq, e);
                }
            }

            sleep(Duration::from_secs(1)).await;
        }

        print_final_stats(&self.target, &stats);
        Ok(())
    }

    async fn resolve_target(&self) -> Result<SocketAddr> {
        let host_port = if self.target.contains(':') {
            self.target.clone()
        } else {
            format!("{}:4444", self.target)
        };

        // Could optimize with `lazy_static` or `OnceCell` caching
        let addr = tokio::task::spawn_blocking(move || {
            host_port
                .to_socket_addrs()?
                .next()
                .ok_or_else(|| anyhow::anyhow!("Failed to resolve hostname"))
        })
        .await??;

        Ok(addr)
    }

    async fn ping_once(&self, addr: SocketAddr, sequence: u64) -> Result<usize> {
        let stream =
            tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await??;

        let mut stream = stream;

        // Could benefit from memoizing serialized string
        let request = StratumRequest {
            id: sequence,
            method: "mining.subscribe".to_string(),
            params: vec![json!("para-ping/1.0")],
        };

        // Change to single buffer?
        let request_json = serde_json::to_string(&request)?;
        let request_line = format!("{}\n", request_json);

        stream.write_all(request_line.as_bytes()).await?;

        let mut reader = BufReader::new(&mut stream);
        let mut response_line = String::new();
        let bytes_read = reader.read_line(&mut response_line).await?;

        let _: serde_json::Value = serde_json::from_str(response_line.trim())?;

        Ok(bytes_read)
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

        // Not sure whether to use a tuple or struct here
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

    println!("\n--- {} ping statistics ---", target);
    println!(
        "{} packets transmitted, {} received, {:.1}% packet loss",
        sent, received, loss_percent
    );

    if received > 0 {
        println!(
            "round-trip min/avg/max = {:.3}/{:.3}/{:.3} ms",
            min_ms, avg_ms, max_ms
        );
    }
}
