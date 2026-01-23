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
    _path: PathBuf,
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
            _path: path,
            format,
            writer: Arc::new(tokio::sync::Mutex::new(Some(writer))),
        })
    }

    pub async fn _json_lines(path: PathBuf) -> Result<Self> {
        Self::new(path, FileFormat::JsonLines).await
    }

    pub async fn _csv(path: PathBuf) -> Result<Self> {
        Self::new(path, FileFormat::Csv).await
    }

    async fn write_event(&self, event: &Event) -> Result<u64> {
        let mut guard = self.writer.lock().await;
        let writer = guard.as_mut().ok_or_else(|| anyhow!("FileSink closed"))?;

        match self.format {
            FileFormat::JsonLines => {
                let json = serde_json::to_string(event)?;
                writer.write_all(json.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
            FileFormat::Csv => {
                let line = self.event_to_csv(event);
                writer.write_all(line.as_bytes()).await?;
                writer.write_all(b"\n").await?;
            }
        }
        Ok(1)
    }

    fn event_to_csv(&self, event: &Event) -> String {
        fn quote_if_needed(s: &str) -> String {
            if s.contains(',') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        }

        match event {
            Event::Share(s) => {
                format!(
                    "{},{},{},{},{},{},{},{}",
                    s.timestamp,
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
                format!(
                    "{},{},{},{},{},{},{}",
                    b.timestamp,
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
