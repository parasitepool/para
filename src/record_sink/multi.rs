use super::{RecordSink, Result, async_trait, event::Event};

pub struct MultiSink {
    sinks: Vec<Box<dyn RecordSink>>,
}

impl MultiSink {
    pub fn new(sinks: Vec<Box<dyn RecordSink>>) -> Self {
        Self { sinks }
    }
}

#[async_trait]
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
