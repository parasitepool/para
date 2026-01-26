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
    async fn record(&mut self, event: Event) -> Result<u64> {
        let mut updated_records = 0;
        for sink in &mut self.sinks {
            updated_records = updated_records.max(sink.record(event.clone()).await?);
        }
        Ok(updated_records)
    }

    async fn flush(&mut self) -> Result<()> {
        for sink in &mut self.sinks {
            sink.flush().await?;
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        for sink in &mut self.sinks {
            sink.close().await?;
        }
        Ok(())
    }
}
