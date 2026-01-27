use {
    std::{
        collections::VecDeque,
        sync::{Arc, Mutex, OnceLock},
    },
    tokio::sync::broadcast,
    tracing::Subscriber,
    tracing_subscriber::Layer,
};

const BACKLOG_SIZE: usize = 30;

static LOG_SUBSCRIBER: OnceLock<LogSubscriber> = OnceLock::new();

pub struct LogBroadcastLayer {
    tx: broadcast::Sender<Arc<str>>,
    backlog: Arc<Mutex<VecDeque<Arc<str>>>>,
}

#[derive(Clone)]
pub struct LogSubscriber {
    tx: broadcast::Sender<Arc<str>>,
    backlog: Arc<Mutex<VecDeque<Arc<str>>>>,
}

impl LogSubscriber {
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.tx.subscribe()
    }

    pub fn backlog(&self) -> Vec<Arc<str>> {
        self.backlog.lock().unwrap().iter().cloned().collect()
    }
}

pub fn init(capacity: usize) -> LogBroadcastLayer {
    let (tx, _) = broadcast::channel(capacity);
    let backlog = Arc::new(Mutex::new(VecDeque::with_capacity(BACKLOG_SIZE)));
    let subscriber = LogSubscriber {
        tx: tx.clone(),
        backlog: backlog.clone(),
    };
    LOG_SUBSCRIBER.set(subscriber).ok();
    LogBroadcastLayer { tx, backlog }
}

pub fn subscriber() -> Option<LogSubscriber> {
    LOG_SUBSCRIBER.get().cloned()
}

impl<S> Layer<S> for LogBroadcastLayer
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
        {
            let mut backlog = self.backlog.lock().unwrap();
            if backlog.len() == BACKLOG_SIZE {
                backlog.pop_front();
            }
            backlog.push_back(formatted.clone());
        }
        let _ = self.tx.send(formatted);
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
