use {
    super::{Result, async_trait, event::*},
    anyhow::anyhow,
    std::{path::PathBuf, sync::Arc},
    tokio::{
        fs::OpenOptions,
        io::{AsyncWriteExt, BufWriter},
    },
};

#[derive(Debug, Clone, Copy, Default)]
pub enum FileFormat {
    #[default]
    JsonLines,
    Csv,
}

pub struct FileSink {
    format: FileFormat,
    writer: Arc<tokio::sync::Mutex<Option<BufWriter<tokio::fs::File>>>>,
}

impl FileSink {
    pub async fn new(path: PathBuf, format: FileFormat) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        let writer = BufWriter::new(file);

        Ok(Self {
            format,
            writer: Arc::new(tokio::sync::Mutex::new(Some(writer))),
        })
    }

    async fn write_event(&self, event: &Event) -> Result<u64> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = || -> i64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
        };

        let mut guard = self.writer.lock().await;
        let writer = guard.as_mut().ok_or_else(|| anyhow!("FileSink closed"))?;

        match self.format {
            FileFormat::JsonLines => {
                let mut event_with_timestamp = event.clone();
                match &mut event_with_timestamp {
                    Event::Share(s) if s.timestamp.is_none() => s.timestamp = Some(now()),
                    Event::BlockFound(b) if b.timestamp.is_none() => b.timestamp = Some(now()),
                    _ => {}
                }
                let json = serde_json::to_string(&event_with_timestamp)?;
                writer.write_all(json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
            FileFormat::Csv => {
                let line = self.event_to_csv(event, now());
                writer.write_all(line.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
        }
        Ok(1)
    }

    fn event_to_csv(&self, event: &Event, default_timestamp: i64) -> String {
        fn quote_if_needed(s: &str) -> String {
            if s.contains(',') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        }

        match event {
            Event::Share(s) => {
                let timestamp = s.timestamp.unwrap_or(default_timestamp);
                format!(
                    "{},{},{},{},{},{},{},{}",
                    timestamp,
                    "share",
                    quote_if_needed(&s.address),
                    quote_if_needed(&s.workername),
                    s.pool_diff,
                    s.share_diff,
                    s.result,
                    s.reject_reason
                        .as_ref()
                        .map(|r| quote_if_needed(r))
                        .unwrap_or_default()
                )
            }
            Event::BlockFound(b) => {
                let timestamp = b.timestamp.unwrap_or(default_timestamp);
                format!(
                    "{},{},{},{},{},{},{}",
                    timestamp,
                    "block_found",
                    quote_if_needed(&b.address),
                    quote_if_needed(&b.workername),
                    b.blockheight,
                    quote_if_needed(&b.blockhash),
                    b.diff
                )
            }
        }
    }
}

#[async_trait]
impl super::RecordSink for FileSink {
    async fn record(&self, event: Event) -> Result<u64> {
        self.write_event(&event).await
    }

    async fn flush(&self) -> Result<()> {
        let mut guard = self.writer.lock().await;
        if let Some(writer) = guard.as_mut() {
            writer.flush().await?;
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        let mut guard = self.writer.lock().await;
        if let Some(mut writer) = guard.take() {
            writer.flush().await?;
        }
        Ok(())
    }
}
