use {
    super::{Result, async_trait, event::*},
    std::path::PathBuf,
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
    writer: BufWriter<tokio::fs::File>,
}

impl FileSink {
    pub async fn new(path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        let writer = BufWriter::new(file);

        let format = if path.extension().is_some_and(|e| e == "csv") {
            FileFormat::Csv
        } else {
            FileFormat::JsonLines
        };

        Ok(Self { format, writer })
    }

    async fn write_event(&mut self, event: &Event) -> Result<u64> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now = || -> i64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
        };

        match self.format {
            FileFormat::JsonLines => {
                let mut event_with_timestamp = event.clone();
                match &mut event_with_timestamp {
                    Event::Share(s) if s.timestamp.is_none() => s.timestamp = Some(now()),
                    Event::BlockFound(b) if b.timestamp.is_none() => b.timestamp = Some(now()),
                    _ => {}
                }
                let json = serde_json::to_string(&event_with_timestamp)?;
                self.writer.write_all(json.as_bytes()).await?;
                self.writer.write_all(b"\n").await?;
            }
            FileFormat::Csv => {
                let line = self.event_to_csv(event, now());
                self.writer.write_all(line.as_bytes()).await?;
                self.writer.write_all(b"\n").await?;
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
impl super::EventSink for FileSink {
    async fn record(&mut self, event: Event) -> Result<u64> {
        self.write_event(&event).await
    }

    async fn flush(&mut self) -> Result<()> {
        self.writer.flush().await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        self.writer.flush().await?;
        Ok(())
    }
}
