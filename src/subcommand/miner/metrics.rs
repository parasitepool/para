use {super::*, crossterm::terminal};

#[derive(Clone)]
pub(crate) struct Metrics {
    total: Arc<AtomicU64>,
    started: Instant,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            total: Arc::new(AtomicU64::new(0)),
            started: Instant::now(),
        }
    }

    pub fn add(&self, hashes: u64) {
        self.total.fetch_add(hashes, Ordering::Relaxed);
    }

    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }
}

pub async fn spawn_status_line(metrics: Metrics, period: Duration) {
    let frames = ["⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽", "⣾"];
    let mut idx = 0;
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut prev_time = Instant::now();
    let mut prev_total = metrics.total();
    let mut sticky = Sticky::new().expect("tty");

    loop {
        ticker.tick().await;

        let now = Instant::now();
        let total = metrics.total();

        let dt = now.duration_since(prev_time).as_secs_f64().max(1e-6);
        let delta = total.saturating_sub(prev_total) as f64;
        let hash_rate = delta / dt;

        let spinner = frames[idx % frames.len()];
        idx = idx.wrapping_add(1);

        let line = format!(
            " {spinner}  hashrate={}  uptime={:.1}s",
            ckpool::HashRate(hash_rate),
            metrics.uptime().as_secs_f64()
        );

        let _ = sticky.redraw(&line);

        prev_time = now;
        prev_total = total;
    }
}

struct Sticky {
    rows: u16,
}

impl Sticky {
    fn new() -> io::Result<Self> {
        let (_, rows) = terminal::size()?;
        let mut out = io::stdout();
        write!(out, "\x1b[1;{}r\x1b[?25l", rows.saturating_sub(1))?;
        out.flush()?;

        Ok(Self { rows })
    }

    fn redraw(&mut self, line: &str) -> io::Result<()> {
        if let Ok((_, rows)) = terminal::size()
            && rows != self.rows
        {
            self.rows = rows;
            let mut out = io::stdout();
            write!(out, "\x1b[r\x1b[1;{}r", rows.saturating_sub(1))?;
            out.flush()?;
        }

        let mut out = io::stdout();
        write!(out, "\x1b7\x1b[{};1H\x1b[2K{}\x1b8", self.rows, line)?;
        out.flush()
    }
}

impl Drop for Sticky {
    fn drop(&mut self) {
        let _ = write!(io::stdout(), "\x1b[r\x1b[2K\x1b[?25h");
        let _ = io::stdout().flush();
    }
}
