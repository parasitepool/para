use super::*;

const BACKLOG_SIZE: usize = 100;
const CHANNEL_CAPACITY: usize = 1000;

static BACKLOG: Mutex<VecDeque<Arc<str>>> = Mutex::new(VecDeque::new());
static CURRENT_LEVEL: Mutex<String> = Mutex::new(String::new());
static RELOAD: OnceLock<Box<dyn Fn(EnvFilter) -> Result<()> + Send + Sync>> = OnceLock::new();
static TX: LazyLock<broadcast::Sender<Arc<str>>> =
    LazyLock::new(|| broadcast::channel(CHANNEL_CAPACITY).0);

pub(crate) fn logs_enabled() -> bool {
    std::env::var_os("RUST_LOG").is_some()
}

pub(crate) fn init() -> tracing_appender::non_blocking::WorkerGuard {
    let (writer, guard) = non_blocking(io::stderr());

    let fmt_filter = EnvFilter::from_default_env();

    let ls_filter = EnvFilter::new("warn,para=info");
    let (ls_filter, ls_reload_handle) = tracing_subscriber::reload::Layer::new(ls_filter);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_writer(writer)
                .with_filter(fmt_filter),
        )
        .with(StreamLayer.with_filter(ls_filter))
        .init();

    *CURRENT_LEVEL.lock() = String::from("info");

    assert!(
        RELOAD
            .set(Box::new(move |f: EnvFilter| {
                ls_reload_handle
                    .reload(f)
                    .context("failed to reload logstream filter")
            }))
            .is_ok()
    );

    guard
}

pub(crate) fn set_level(level: &str) -> Result<()> {
    let reload = RELOAD.get().context("tracing not initialized")?;

    reload(EnvFilter::new(format!("warn,para={level}")))?;

    *CURRENT_LEVEL.lock() = level.to_string();

    info!("Log level changed to: {}", level);
    Ok(())
}

pub(crate) fn get_level() -> String {
    CURRENT_LEVEL.lock().clone()
}

pub(crate) fn subscribe() -> broadcast::Receiver<Arc<str>> {
    TX.subscribe()
}

pub(crate) fn backlog() -> Vec<Arc<str>> {
    BACKLOG.lock().iter().cloned().collect()
}

pub(crate) fn broadcast_level(level: &str) {
    let msg: Arc<str> = format!("level\t{level}").into();
    let _ = TX.send(msg);
}

struct StreamLayer;

impl<S> Layer<S> for StreamLayer
where
    S: Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);

        let level = event.metadata().level();
        let message = visitor.message.unwrap_or_default();

        let formatted: Arc<str> = format!("{level:>5}\t{message}").into();
        let mut backlog = BACKLOG.lock();
        if backlog.len() == BACKLOG_SIZE {
            backlog.pop_front();
        }
        backlog.push_back(formatted.clone());
        let _ = TX.send(formatted);
    }
}

#[derive(Default)]
struct LogVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for LogVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{:?}", value));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }
}
