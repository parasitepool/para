use {
    super::*,
    parking_lot::Mutex,
    std::collections::VecDeque,
    std::sync::{Arc, LazyLock},
    tokio::sync::broadcast,
};

const BACKLOG_SIZE: usize = 30;
const CHANNEL_CAPACITY: usize = 1000;

static TX: LazyLock<broadcast::Sender<Arc<str>>> =
    LazyLock::new(|| broadcast::channel(CHANNEL_CAPACITY).0);

static BACKLOG: Mutex<VecDeque<Arc<str>>> = Mutex::new(VecDeque::new());

pub fn subscribe() -> broadcast::Receiver<Arc<str>> {
    TX.subscribe()
}

pub fn backlog() -> Vec<Arc<str>> {
    BACKLOG.lock().iter().cloned().collect()
}

pub struct LogStreamLayer;

impl<S> Layer<S> for LogStreamLayer
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
