use super::*;

pub(crate) struct Metatron {
    blocks: AtomicU64,
    shares: AtomicU64,
    started: Instant,
    workers: AtomicU64,
}

impl Metatron {
    pub(crate) fn new() -> Self {
        Self {
            blocks: AtomicU64::new(0),
            shares: AtomicU64::new(0),
            started: Instant::now(),
            workers: AtomicU64::new(0),
        }
    }

    pub(crate) fn add_block(&self) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn add_share(&self) {
        self.shares.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn add_worker(&self) {
        self.workers.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn sub_worker(&self) {
        self.workers.fetch_sub(1, Ordering::Relaxed);
    }

    pub(crate) fn total_blocks(&self) -> u64 {
        self.blocks.load(Ordering::Relaxed)
    }

    pub(crate) fn total_shares(&self) -> u64 {
        self.shares.load(Ordering::Relaxed)
    }

    pub(crate) fn total_workers(&self) -> u64 {
        self.workers.load(Ordering::Relaxed)
    }

    pub(crate) fn uptime(&self) -> Duration {
        self.started.elapsed()
    }
}

struct Throbber;

impl Throbber {
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

impl Drop for Throbber {
    fn drop(&mut self) {
        let _ = write!(io::stdout(), "\x1b[u\x1b[2K\r\n");
        let _ = io::stdout().flush();
    }
}

pub(crate) fn spawn_throbber(metatron: Arc<Metatron>) {
    tokio::spawn(async move {
        let frames = ["⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽", "⣾"];
        let mut frame = 0;
        let mut ticker = interval(Duration::from_millis(200));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let anchor = Throbber::new().expect("tty");

        loop {
            ticker.tick().await;

            let throbber = frames[frame % frames.len()];
            frame = frame.wrapping_add(1);

            let line = format!(
                " {throbber}  workers={}  shares={}  blocks={}  uptime={}s",
                metatron.total_workers(),
                metatron.total_shares(),
                metatron.total_blocks(),
                metatron.uptime().as_secs()
            );

            let _ = anchor.redraw(&line);
        }
    });
}
