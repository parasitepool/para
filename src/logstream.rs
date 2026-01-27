use {super::*, std::sync::Mutex};

const BACKLOG_SIZE: usize = 30;
const CHANNEL_CAPACITY: usize = 1000;

static LOGSTREAM: LazyLock<LogStream> = LazyLock::new(|| {
    let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
    let backlog = Arc::new(Mutex::new(VecDeque::with_capacity(BACKLOG_SIZE)));

    LogStream { tx, backlog }
});

#[derive(Clone)]
pub struct LogStream {
    tx: broadcast::Sender<Arc<str>>,
    backlog: Arc<Mutex<VecDeque<Arc<str>>>>,
}

impl LogStream {
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.tx.subscribe()
    }

    pub fn backlog(&self) -> Vec<Arc<str>> {
        self.backlog.lock().unwrap().iter().cloned().collect()
    }
}

pub fn get() -> &'static LogStream {
    &LOGSTREAM
}

impl<S> Layer<S> for &'static LogStream
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
