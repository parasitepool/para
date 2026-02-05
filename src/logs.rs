use super::*;

const BACKLOG_SIZE: usize = 100;
const CHANNEL_CAPACITY: usize = 1000;

enum Msg {
    Event(tracing::Level, String),
    SetLevel(String),
}

pub(crate) struct Logs {
    tx: std::sync::mpsc::Sender<Msg>,
    backlog: Mutex<VecDeque<Arc<str>>>,
    level: Mutex<String>,
    broadcast_tx: broadcast::Sender<Arc<str>>,
}

impl Logs {
    pub(crate) fn set_level(&self, level: &str) {
        let _ = self.tx.send(Msg::SetLevel(level.to_string()));
        *self.level.lock() = level.to_string();
    }

    pub(crate) fn get_level(&self) -> String {
        self.level.lock().clone()
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.broadcast_tx.subscribe()
    }

    pub(crate) fn backlog(&self) -> Vec<Arc<str>> {
        self.backlog.lock().iter().cloned().collect()
    }

    pub(crate) fn broadcast_level(&self, level: &str) {
        let msg: Arc<str> = format!("level\t{level}").into();
        let _ = self.broadcast_tx.send(msg);
    }
}

pub(crate) fn logs_enabled() -> bool {
    std::env::var_os("RUST_LOG").is_some()
}

pub(crate) fn init() -> (Arc<Logs>, tracing_appender::non_blocking::WorkerGuard) {
    let (writer, guard) = non_blocking(io::stderr());
    let (tx, rx) = std::sync::mpsc::channel();
    let (broadcast_tx, _) = broadcast::channel(CHANNEL_CAPACITY);

    let fmt_filter = EnvFilter::from_default_env();

    let ls_filter = EnvFilter::new("warn,para=info");
    let (ls_filter, reload_handle) = tracing_subscriber::reload::Layer::new(ls_filter);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_writer(writer)
                .with_filter(fmt_filter),
        )
        .with(StreamLayer { tx: tx.clone() }.with_filter(ls_filter))
        .init();

    let logs = Arc::new(Logs {
        tx,
        backlog: Mutex::new(VecDeque::new()),
        level: Mutex::new(String::from("info")),
        broadcast_tx,
    });

    let logs_bg = logs.clone();
    thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            match msg {
                Msg::Event(level, message) => {
                    let formatted: Arc<str> = format!("{level:>5}\t{message}").into();
                    let mut backlog = logs_bg.backlog.lock();
                    if backlog.len() == BACKLOG_SIZE {
                        backlog.pop_front();
                    }
                    backlog.push_back(formatted.clone());
                    let _ = logs_bg.broadcast_tx.send(formatted);
                }
                Msg::SetLevel(level) => {
                    let filter = EnvFilter::new(format!("warn,para={level}"));
                    if let Err(e) = reload_handle.reload(filter) {
                        warn!("Failed to reload log filter: {e}");
                    }
                }
            }
        }
    });

    (logs, guard)
}

struct StreamLayer {
    tx: std::sync::mpsc::Sender<Msg>,
}

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

        let level = *event.metadata().level();
        let message = visitor.message.unwrap_or_default();

        let _ = self.tx.send(Msg::Event(level, message));
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
