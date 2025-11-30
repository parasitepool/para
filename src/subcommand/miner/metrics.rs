use super::*;

pub(crate) struct Metrics {
    hashes: AtomicU64,
    shares: AtomicU64,
    started: Instant,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            hashes: AtomicU64::new(0),
            shares: AtomicU64::new(0),
            started: Instant::now(),
        }
    }

    pub fn add_hashes(&self, hashes: u64) {
        self.hashes.fetch_add(hashes, Ordering::Relaxed);
    }

    pub fn add_share(&self) {
        self.shares.fetch_add(1, Ordering::Relaxed);
    }

    pub fn total_hashes(&self) -> u64 {
        self.hashes.load(Ordering::Relaxed)
    }

    pub fn total_shares(&self) -> u64 {
        self.shares.load(Ordering::Relaxed)
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }
}

struct Anchored;

impl Anchored {
    fn new() -> io::Result<Self> {
        let mut out = io::stdout();
        writeln!(out)?;
        write!(out, "\x1b[s")?;
        out.flush()?;
        Ok(Self)
    }
    fn redraw(&self, line: &str) -> io::Result<()> {
        let mut out = io::stdout();
        write!(out, "\x1b[u\x1b[2K\r{}", line)?;
        out.flush()
    }
}

impl Drop for Anchored {
    fn drop(&mut self) {
        let _ = write!(io::stdout(), "\x1b[u\x1b[2K\r\n");
        let _ = io::stdout().flush();
    }
}

pub(crate) fn spawn_throbber(metrics: Arc<Metrics>) {
    tokio::spawn(async move {
        let frames = ["⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽", "⣾"];
        let mut frame = 0;
        let mut ticker = tokio::time::interval(Duration::from_millis(200));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut prev_time = Instant::now();
        let mut prev_hashes = metrics.total_hashes();
        let mut prev_shares = metrics.total_shares();
        let mut smooth_hashrate: Option<f64> = None;
        let mut smooth_sps: Option<f64> = None;
        let anchor = Anchored::new().expect("tty");

        loop {
            ticker.tick().await;

            let now = Instant::now();
            let total_hashes = metrics.total_hashes();
            let total_shares = metrics.total_shares();

            let dt = now.duration_since(prev_time).as_secs_f64().max(1e-6);

            // Instantaneous hashrate
            let hash_delta = total_hashes.saturating_sub(prev_hashes) as f64;
            let inst_hashrate = hash_delta / dt;

            // Instantaneous shares per second
            let share_delta = total_shares.saturating_sub(prev_shares) as f64;
            let inst_sps = share_delta / dt;

            // alpha = 1 - e^(-dt/tau) gives time-based smoothing (EMA)
            let tau = 3.0;
            let alpha = 1.0 - (-dt / tau).exp();

            // Smooth hashrate
            let hashrate = match smooth_hashrate {
                None => {
                    smooth_hashrate = Some(inst_hashrate);
                    inst_hashrate
                }
                Some(s) => {
                    let v = s + alpha * (inst_hashrate - s);
                    smooth_hashrate = Some(v);
                    v
                }
            };

            // Smooth SPS (use longer tau for shares since they're less frequent)
            let sps_tau = 10.0;
            let sps_alpha = 1.0 - (-dt / sps_tau).exp();
            let sps = match smooth_sps {
                None => {
                    smooth_sps = Some(inst_sps);
                    inst_sps
                }
                Some(s) => {
                    let v = s + sps_alpha * (inst_sps - s);
                    smooth_sps = Some(v);
                    v
                }
            };

            let throbber = frames[frame % frames.len()];
            frame = frame.wrapping_add(1);

            // Format SPS: show as "X.XX/s" or "Xm Ys" for interval
            let sps_display = if sps > 0.0 {
                let interval = 1.0 / sps;
                if interval < 60.0 {
                    format!("{:.2}/s (1 per {:.1}s)", sps, interval)
                } else {
                    let mins = (interval / 60.0).floor() as u64;
                    let secs = (interval % 60.0).floor() as u64;
                    format!("{:.2}/s (1 per {}m{}s)", sps, mins, secs)
                }
            } else {
                "0.00/s".to_string()
            };

            let line = format!(
                " {throbber}  hashrate={}H/s  shares={} sps={}  uptime={:.0}s",
                ckpool::HashRate(hashrate),
                total_shares,
                sps_display,
                metrics.uptime().as_secs_f64()
            );

            let mut out = io::stdout();
            let _ = write!(out, "\x1b[2K\r{line}");
            let _ = out.flush();

            let _ = anchor.redraw(&line);

            prev_time = now;
            prev_hashes = total_hashes;
            prev_shares = total_shares;
        }
    });
}
