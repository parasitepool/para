use super::*;

pub(crate) trait Source: Clone + Send + Sync + 'static {
    fn create_job(
        self: &Arc<Self>,
        enonce1: &Extranonce,
        enonce2_size: usize,
        address: Option<&Address>,
        job_id: JobId,
        version_mask: Option<Version>,
    ) -> Job<Self>
    where
        Self: Sized;

    fn clean_jobs(&self, prev: Option<&Self>) -> bool;

    fn build_block(&self, job: &Job<Self>, submit: &Submit, header: Header) -> Option<Block>
    where
        Self: Sized;
}

impl Source for Workbase<BlockTemplate> {
    fn create_job(
        self: &Arc<Self>,
        enonce1: &Extranonce,
        enonce2_size: usize,
        address: Option<&Address>,
        job_id: JobId,
        version_mask: Option<Version>,
    ) -> Job<Self> {
        let address = address.expect("pool mode requires address");
        let template = self.template();

        let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address.clone(),
            enonce1.clone(),
            enonce2_size,
            template.height,
            template.coinbase_value,
            template.default_witness_commitment.clone(),
        )
        .with_aux(template.coinbaseaux.clone())
        .with_timestamp(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .with_pool_sig("|parasite|".into())
        .build()
        .expect("coinbase build failed");

        Job {
            job_id,
            prevhash: template.previous_block_hash.into(),
            coinb1,
            coinb2,
            merkle_branches: self.merkle_branches().to_vec(),
            version: template.version,
            nbits: template.bits,
            ntime: template.current_time,
            enonce1: enonce1.clone(),
            version_mask,
            workbase: self.clone(),
        }
    }

    fn clean_jobs(&self, prev: Option<&Self>) -> bool {
        prev.map(|prev| prev.template().height != self.template().height)
            .unwrap_or(true)
    }

    fn build_block(&self, job: &Job<Self>, submit: &Submit, header: Header) -> Option<Block> {
        let coinbase_bin = hex::decode(format!(
            "{}{}{}{}",
            job.coinb1, job.enonce1, submit.enonce2, job.coinb2,
        ))
        .expect("hex decode failed");

        let mut cursor = bitcoin::io::Cursor::new(&coinbase_bin);
        let coinbase_tx = Transaction::consensus_decode_from_finite_reader(&mut cursor)
            .expect("coinbase decode failed");

        let txdata = std::iter::once(coinbase_tx)
            .chain(
                self.template()
                    .transactions
                    .iter()
                    .map(|tx| tx.transaction.clone()),
            )
            .collect();

        let block = Block { header, txdata };

        let height = self.template().height;
        if height > 16 {
            assert!(block.bip34_block_height().is_ok());
        }

        Some(block)
    }
}
