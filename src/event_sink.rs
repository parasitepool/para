use {super::*, tokio::sync::mpsc};

mod database;
mod event;
mod file;
mod multi;

pub use {
    database::DatabaseSink,
    event::{BlockFoundEvent, Event, ShareEvent},
    file::FileSink,
    multi::MultiSink,
};

const EVENT_CHANNEL_CAPACITY: usize = 10_000;

pub(crate) async fn build_event_sink(
    settings: &Settings,
    cancel_token: CancellationToken,
    tasks: &mut JoinSet<()>,
) -> Result<Option<mpsc::Sender<Event>>> {
    let mut sinks: Vec<Box<dyn EventSink>> = Vec::new();

    if let Some(db_url) = settings.database_url() {
        match DatabaseSink::connect(&db_url).await {
            Ok(db_sink) => {
                info!("Database sink connected to {}", db_url);
                sinks.push(Box::new(db_sink));
            }
            Err(e) => {
                warn!("Failed to connect database sink: {e}");
            }
        }
    }

    if let Some(events_file) = settings.events_file() {
        match FileSink::new(events_file.clone()).await {
            Ok(file_sink) => {
                info!("File sink writing to {}", events_file.display());
                sinks.push(Box::new(file_sink));
            }
            Err(e) => {
                warn!("Failed to create file sink: {e}");
            }
        }
    }

    if sinks.is_empty() {
        return Ok(None);
    }

    let sink: Box<dyn EventSink> = if sinks.len() == 1 {
        sinks.remove(0)
    } else {
        Box::new(MultiSink::new(sinks))
    };

    let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

    tasks.spawn(async move {
        let mut rx = rx;
        let mut sink = sink;
        let cancel = cancel_token;

        loop {
            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    while let Ok(event) = rx.try_recv() {
                        if let Err(e) = sink.record(event).await {
                            warn!("Error recording event during shutdown: {e}");
                        }
                    }
                    if let Err(e) = sink.close().await {
                        warn!("Error closing sink: {e}");
                    }
                    break;
                }

                Some(event) = rx.recv() => {
                    if let Err(e) = sink.record(event).await {
                        warn!("Error recording event: {e}");
                    }
                }
            }
        }
    });

    Ok(Some(tx))
}

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn record(&mut self, event: Event) -> Result<u64>;

    async fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        self.flush().await
    }
}

#[macro_export]
macro_rules! rejection_event {
    ($address:expr, $workername:expr, $blockheight:expr, $error:expr) => {
        $crate::event_sink::Event::Share($crate::event_sink::ShareEvent {
            timestamp: None,
            address: $address,
            workername: $workername,
            pool_diff: 0.0,
            share_diff: 0.0,
            result: false,
            blockheight: Some($blockheight),
            reject_reason: Some($error.to_string()),
        })
    };
    ($address:expr, $workername:expr, $pool_diff:expr, $share_diff:expr, $blockheight:expr, $error:expr) => {
        $crate::event_sink::Event::Share($crate::event_sink::ShareEvent {
            timestamp: None,
            address: $address,
            workername: $workername,
            pool_diff: $pool_diff,
            share_diff: $share_diff,
            result: false,
            blockheight: Some($blockheight),
            reject_reason: Some($error.to_string()),
        })
    };
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::sync::atomic::{AtomicU64, Ordering},
    };

    #[tokio::test]
    async fn multi_sink_broadcasts_to_all() {
        let event = Event::Share(ShareEvent {
            timestamp: None,
            address: "bc1test".into(),
            workername: "rig1".into(),
            pool_diff: 1.0,
            share_diff: 1.5,
            result: true,
            blockheight: Some(800000),
            reject_reason: None,
        });

        struct CountingSink {
            count: Arc<AtomicU64>,
        }

        #[async_trait]
        impl EventSink for CountingSink {
            async fn record(&mut self, _event: Event) -> Result<u64> {
                self.count.fetch_add(1, Ordering::Relaxed);
                Ok(1)
            }
        }

        let count1 = Arc::new(AtomicU64::new(0));
        let count2 = Arc::new(AtomicU64::new(0));

        let mut sink = MultiSink::new(vec![
            Box::new(CountingSink {
                count: count1.clone(),
            }),
            Box::new(CountingSink {
                count: count2.clone(),
            }),
        ]);

        sink.record(event).await.unwrap();

        assert_eq!(count1.load(Ordering::Relaxed), 1);
        assert_eq!(count2.load(Ordering::Relaxed), 1);
    }
}
