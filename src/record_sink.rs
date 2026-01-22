use async_trait::async_trait;
use {
    super::*,
    sqlx::{Pool, Postgres},
    std::{
        fs::{File, OpenOptions},
        io::{BufWriter, Write},
        path::PathBuf,
    },
    tokio::sync::mpsc,
};

const EVENT_CHANNEL_CAPACITY: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Share(ShareEvent),
    BlockFound(BlockFoundEvent),
    UserAuthorized(UserAuthorizedEvent),
    WorkerConnected(WorkerConnectedEvent),
    WorkerDisconnected(WorkerDisconnectedEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareEvent {
    pub timestamp: i64,
    pub address: String,
    pub workername: String,
    pub pool_diff: f64,
    pub share_diff: f64,
    pub result: bool,
    pub blockheight: Option<i32>,
    pub reject_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockFoundEvent {
    pub timestamp: i64,
    pub blockheight: i32,
    pub blockhash: String,
    pub address: String,
    pub workername: String,
    pub diff: f64,
    pub coinbase_value: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAuthorizedEvent {
    pub timestamp: i64,
    pub address: String,
    pub workername: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConnectedEvent {
    pub timestamp: i64,
    pub address: String,
    pub workername: String,
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerDisconnectedEvent {
    pub timestamp: i64,
    pub address: String,
    pub workername: String,
    pub duration_secs: u64,
    pub shares_submitted: u64,
}

impl Event {
    pub fn _timestamp(&self) -> i64 {
        match self {
            Event::Share(e) => e.timestamp,
            Event::BlockFound(e) => e.timestamp,
            Event::UserAuthorized(e) => e.timestamp,
            Event::WorkerConnected(e) => e.timestamp,
            Event::WorkerDisconnected(e) => e.timestamp,
        }
    }

    pub fn _event_type(&self) -> &'static str {
        match self {
            Event::Share(_) => "share",
            Event::BlockFound(_) => "block_found",
            Event::UserAuthorized(_) => "user_authorized",
            Event::WorkerConnected(_) => "worker_connected",
            Event::WorkerDisconnected(_) => "worker_disconnected",
        }
    }
}

/// Builds a record sink from settings configuration.
/// Returns None if no sinks are configured.
/// Returns Some with sender if sinks are configured.
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

        match FileSink::new(events_file.clone(), format) {
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
    let sink_cancel = CancellationToken::new();
    let handle = spawn_sink_consumer(rx, sink, sink_cancel.clone());

    tasks.spawn(async move {
        let _ = handle.await;
    });

    tasks.spawn({
        let cancel_token = cancel_token.clone();
        async move {
            cancel_token.cancelled().await;
            sink_cancel.cancel();
        }
    });

    Ok(Some(tx))
}

/// Trait for consuming and storing pool events
#[async_trait]
pub trait RecordSink: Send + Sync {
    async fn record(&self, event: Event) -> Result<u64>;

    async fn _record_batch(&self, events: Vec<Event>) -> Result<()> {
        for event in events {
            self.record(event).await?;
        }
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.flush().await
    }
}

pub struct DatabaseSink {
    pool: Pool<Postgres>,
}

impl DatabaseSink {
    pub fn _new(pool: Pool<Postgres>) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }
}

#[async_trait::async_trait]
impl RecordSink for DatabaseSink {
    async fn record(&self, event: Event) -> Result<u64> {
        let rows_changed = match event {
            Event::Share(share) => {
                sqlx::query(
                    "INSERT INTO shares (
                        blockheight, diff, sdiff, result, reject_reason,
                        workername, username, createdate
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, to_timestamp($8))",
                )
                    .bind(share.blockheight)
                    .bind(share.pool_diff)
                    .bind(share.share_diff)
                    .bind(share.result)
                    .bind(&share.reject_reason)
                    .bind(&share.workername)
                    .bind(&share.address)
                    .bind(share.timestamp)
                    .execute(&self.pool)
                    .await?
            }
            Event::BlockFound(block) => {
                sqlx::query(
                    "INSERT INTO blocks (
                        blockheight, blockhash, workername, username, diff, coinbasevalue, time_found
                    ) VALUES ($1, $2, $3, $4, $5, $6, to_timestamp($7))",
                )
                    .bind(block.blockheight)
                    .bind(&block.blockhash)
                    .bind(&block.workername)
                    .bind(&block.address)
                    .bind(block.diff)
                    .bind(block.coinbase_value)
                    .bind(block.timestamp)
                    .execute(&self.pool)
                    .await?
            }
            _ => {
                return Ok(0);
            }
        }.rows_affected();
        Ok(rows_changed)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub enum FileFormat {
    #[default]
    JsonLines,
    Csv,
}

pub struct FileSink {
    _path: PathBuf,
    format: FileFormat,
    writer: std::sync::Mutex<Option<BufWriter<File>>>,
}

impl FileSink {
    pub fn new(path: PathBuf, format: FileFormat) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let writer = BufWriter::new(file);

        Ok(Self {
            _path: path,
            format,
            writer: std::sync::Mutex::new(Some(writer)),
        })
    }

    pub fn _json_lines(path: PathBuf) -> Result<Self> {
        Self::new(path, FileFormat::JsonLines)
    }

    pub fn _csv(path: PathBuf) -> Result<Self> {
        Self::new(path, FileFormat::Csv)
    }

    fn write_event(&self, event: &Event) -> Result<u64> {
        let mut guard = self.writer.lock().unwrap();
        let writer = guard.as_mut().ok_or_else(|| anyhow!("FileSink closed"))?;

        match self.format {
            FileFormat::JsonLines => {
                serde_json::to_writer(&mut *writer, event)?;
                writeln!(writer)?;
            }
            FileFormat::Csv => {
                let line = self.event_to_csv(event);
                writeln!(writer, "{}", line)?;
            }
        }
        Ok(1)
    }

    fn event_to_csv(&self, event: &Event) -> String {
        match event {
            Event::Share(s) => {
                format!(
                    "{},{},{},{},{},{},{},{}",
                    s.timestamp,
                    "share",
                    s.address,
                    s.workername,
                    s.pool_diff,
                    s.share_diff,
                    s.result,
                    s.reject_reason.as_deref().unwrap_or("")
                )
            }
            Event::BlockFound(b) => {
                format!(
                    "{},{},{},{},{},{},{}",
                    b.timestamp,
                    "block_found",
                    b.address,
                    b.workername,
                    b.blockheight,
                    b.blockhash,
                    b.diff
                )
            }
            Event::UserAuthorized(u) => {
                format!(
                    "{},{},{},{}",
                    u.timestamp, "user_authorized", u.address, u.workername
                )
            }
            Event::WorkerConnected(w) => {
                format!(
                    "{},{},{},{},{}",
                    w.timestamp,
                    "worker_connected",
                    w.address,
                    w.workername,
                    w.agent.as_deref().unwrap_or("")
                )
            }
            Event::WorkerDisconnected(w) => {
                format!(
                    "{},{},{},{},{},{}",
                    w.timestamp,
                    "worker_disconnected",
                    w.address,
                    w.workername,
                    w.duration_secs,
                    w.shares_submitted
                )
            }
        }
    }
}

#[async_trait::async_trait]
impl RecordSink for FileSink {
    async fn record(&self, event: Event) -> Result<u64> {
        self.write_event(&event)
    }

    async fn flush(&self) -> Result<()> {
        let mut guard = self.writer.lock().unwrap();
        if let Some(writer) = guard.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        let mut guard = self.writer.lock().unwrap();
        if let Some(mut writer) = guard.take() {
            writer.flush()?;
        }
        Ok(())
    }
}

pub struct MultiSink {
    sinks: Vec<Box<dyn RecordSink>>,
}

impl MultiSink {
    pub fn new(sinks: Vec<Box<dyn RecordSink>>) -> Self {
        Self { sinks }
    }
}

#[async_trait::async_trait]
impl RecordSink for MultiSink {
    async fn record(&self, event: Event) -> Result<u64> {
        let mut updated_records = 0;
        for sink in &self.sinks {
            updated_records = updated_records.max(sink.record(event.clone()).await?);
        }
        Ok(updated_records)
    }

    async fn flush(&self) -> Result<()> {
        for sink in &self.sinks {
            sink.flush().await?;
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        for sink in &self.sinks {
            sink.close().await?;
        }
        Ok(())
    }
}

pub struct _NullSink;

#[async_trait::async_trait]
impl RecordSink for _NullSink {
    async fn record(&self, _event: Event) -> Result<u64> {
        Ok(0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn test_share() -> Event {
        Event::Share(ShareEvent {
            timestamp: now(),
            address: "bc1test".into(),
            workername: "rig1".into(),
            pool_diff: 1.0,
            share_diff: 1.5,
            result: true,
            blockheight: Some(800000),
            reject_reason: None,
        })
    }

    fn test_block() -> Event {
        Event::BlockFound(BlockFoundEvent {
            timestamp: now(),
            blockheight: 800000,
            blockhash: "00000000000000000001".into(),
            address: "bc1test".into(),
            workername: "rig1".into(),
            diff: 1000.0,
            coinbase_value: Some(625000000),
        })
    }

    #[test]
    fn event_type_returns_correct_string() {
        assert_eq!(test_share()._event_type(), "share");
        assert_eq!(test_block()._event_type(), "block_found");
    }

    #[test]
    fn event_serializes_to_json() {
        let share = test_share();
        let json = serde_json::to_string(&share).unwrap();
        assert!(json.contains("\"type\":\"share\""));
    }

    #[test]
    fn event_deserializes_from_json() {
        let share = test_share();
        let json = serde_json::to_string(&share).unwrap();
        let parsed: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed._event_type(), "share");
    }

    #[tokio::test]
    async fn null_sink_accepts_events() {
        let sink = _NullSink;
        sink.record(test_share()).await.unwrap();
        sink.record(test_block()).await.unwrap();
    }

    #[tokio::test]
    async fn multi_sink_broadcasts_to_all() {
        let sink = MultiSink::new(vec![Box::new(_NullSink), Box::new(_NullSink)]);
        sink.record(test_share()).await.unwrap();
    }
}
