use super::*;

pub(crate) trait StatusLine: Send + Sync + 'static {
    fn status_line(&self) -> String;
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

pub(crate) fn spawn_throbber<T: StatusLine>(
    source: Arc<T>,
    cancel: CancellationToken,
    tasks: &mut JoinSet<()>,
) {
    tasks.spawn(async move {
        let frames = ["⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽", "⣾"];
        let mut frame = 0;
        let mut ticker = interval(Duration::from_millis(200));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let anchor = Throbber::new().expect("tty");

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let throbber = frames[frame % frames.len()];
                    frame = frame.wrapping_add(1);

                    let line = format!(" {throbber}  {}", source.status_line());
                    let _ = anchor.redraw(&line);
                }
            }
        }
    });
}
