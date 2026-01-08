use super::*;

pub(crate) trait Workbase: Clone + Send + Sync + 'static {
    fn merkle_branches(&self) -> Vec<MerkleNode>;

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

impl Workbase for BlockTemplate {
    fn merkle_branches(&self) -> Vec<MerkleNode> {
        stratum::merkle_branches(self.transactions.iter().map(|tx| tx.txid).collect())
    }

    fn create_job(
        self: &Arc<Self>,
        enonce1: &Extranonce,
        enonce2_size: usize,
        address: Option<&Address>,
        job_id: JobId,
        version_mask: Option<Version>,
    ) -> Job<Self> {
        let address = address.expect("pool mode requires address");

        let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address.clone(),
            enonce1.clone(),
            enonce2_size,
            self.height,
            self.coinbase_value,
            self.default_witness_commitment.clone(),
        )
        .with_aux(self.coinbaseaux.clone())
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
            prevhash: self.previous_block_hash.into(),
            coinb1,
            coinb2,
            merkle_branches: self.merkle_branches(),
            version: self.version,
            nbits: self.bits,
            ntime: self.current_time,
            enonce1: enonce1.clone(),
            version_mask,
            workbase: self.clone(),
        }
    }

    fn clean_jobs(&self, prev: Option<&Self>) -> bool {
        prev.map(|prev| prev.height != self.height).unwrap_or(true)
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
            .chain(self.transactions.iter().map(|tx| tx.transaction.clone()))
            .collect();

        let block = Block { header, txdata };

        if self.height > 16 {
            assert!(block.bip34_block_height().is_ok());
        }

        Some(block)
    }
}

impl Workbase for Notify {
    fn merkle_branches(&self) -> Vec<MerkleNode> {
        self.merkle_branches.clone()
    }

    fn create_job(
        self: &Arc<Self>,
        enonce1: &Extranonce,
        _enonce2_size: usize,
        _address: Option<&Address>,
        job_id: JobId,
        version_mask: Option<Version>,
    ) -> Job<Self> {
        Job {
            job_id,
            prevhash: self.prevhash.clone(),
            coinb1: self.coinb1.clone(),
            coinb2: self.coinb2.clone(),
            merkle_branches: self.merkle_branches(),
            version: self.version,
            nbits: self.nbits,
            ntime: self.ntime,
            enonce1: enonce1.clone(),
            version_mask,
            workbase: self.clone(),
        }
    }

    fn clean_jobs(&self, _prev: Option<&Self>) -> bool {
        self.clean_jobs
    }

    fn build_block(&self, _job: &Job<Self>, _submit: &Submit, _header: Header) -> Option<Block> {
        None
    }
}
