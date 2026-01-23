use {
    std::sync::Arc,
    tokio::{sync::mpsc, task::JoinHandle},
};

mod database;
mod event;
mod file;
mod multi;

pub use {
    database::DatabaseSink,
    event::{BlockFoundEvent, Event, ShareEvent},
    file::{FileFormat, FileSink},
    multi::MultiSink,
};

use super::*;

const EVENT_CHANNEL_CAPACITY: usize = 10_000;

pub(crate) async fn build_record_sink(
    settings: &Settings,
    cancel_token: CancellationToken,
    tasks: &mut JoinSet<()>,
) -> Result<Option<mpsc::Sender<Event>>> {
    let mut sinks: Vec<Box<dyn RecordSink>> = Vec::new();

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
        let format = if events_file.extension().is_some_and(|e| e == "csv") {
            FileFormat::Csv
        } else {
            FileFormat::JsonLines
        };

        match FileSink::new(events_file.clone(), format).await {
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

    let sink: Arc<dyn RecordSink> = if sinks.len() == 1 {
        Arc::from(sinks.remove(0))
    } else {
        Arc::new(MultiSink::new(sinks))
    };

    let (tx, rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let handle = spawn_sink_consumer(rx, sink, cancel_token.clone());

    tasks.spawn(async move {
        let _ = handle.await;
    });

    Ok(Some(tx))
}

#[async_trait]
pub trait RecordSink: Send + Sync {
    async fn record(&self, event: Event) -> Result<u64>;

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.flush().await
    }
}

pub fn spawn_sink_consumer(
    mut rx: mpsc::Receiver<Event>,
    sink: Arc<dyn RecordSink>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
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
    })
}

#[macro_export]
macro_rules! rejection_event {
    ($address:expr, $workername:expr, $blockheight:expr, $reason:expr) => {
        $crate::record_sink::Event::Share($crate::record_sink::ShareEvent {
            timestamp: None,
            address: $address,
            workername: $workername,
            pool_diff: 0.0,
            share_diff: 0.0,
            result: false,
            blockheight: Some($blockheight),
            reject_reason: Some($reason.to_string()),
        })
    };
    ($address:expr, $workername:expr, $pool_diff:expr, $share_diff:expr, $blockheight:expr, $reason:expr) => {
        $crate::record_sink::Event::Share($crate::record_sink::ShareEvent {
            timestamp: None,
            address: $address,
            workername: $workername,
            pool_diff: $pool_diff,
            share_diff: $share_diff,
            result: false,
            blockheight: Some($blockheight),
            reject_reason: Some($reason.to_string()),
        })
    };
}

#[cfg(test)]
mod tests {
    use super::*;

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
            count: Arc<Mutex<u64>>,
        }

        #[async_trait]
        impl RecordSink for CountingSink {
            async fn record(&self, _event: Event) -> Result<u64> {
                let mut count = self.count.lock().await;
                *count += 1;
                Ok(1)
            }
        }

        let count1 = Arc::new(Mutex::new(0));
        let count2 = Arc::new(Mutex::new(0));

        let sink = MultiSink::new(vec![
            Box::new(CountingSink {
                count: count1.clone(),
            }),
            Box::new(CountingSink {
                count: count2.clone(),
            }),
        ]);

        sink.record(event).await.unwrap();

        assert_eq!(*count1.lock().await, 1);
        assert_eq!(*count2.lock().await, 1);
    }
}
