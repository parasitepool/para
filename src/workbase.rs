use super::*;

pub(crate) trait Workbase: Clone + Send + Sync + 'static {
    fn merkle_branches(&self) -> &[MerkleNode];
    fn prevhash(&self) -> PrevHash;
    fn version(&self) -> Version;
    fn nbits(&self) -> Nbits;
    fn ntime(&self) -> Ntime;
    fn height(&self) -> i32;
    fn coinbase_value(&self) -> Option<i64>;

    fn create_job(
        self: &Arc<Self>,
        enonce1: &Extranonce,
        enonce2_size: usize,
        address: Option<&Address>,
        job_id: JobId,
        version_mask: Option<Version>,
    ) -> Result<Job<Self>>
    where
        Self: Sized;

    fn clean_jobs(&self, prev: Option<&Self>) -> bool;

    fn upstream_job_id(&self) -> Option<JobId> {
        None
    }

    fn build_block(&self, job: &Job<Self>, submit: &Submit, header: Header) -> Result<Block>
    where
        Self: Sized;
}

impl Workbase for BlockTemplate {
    fn merkle_branches(&self) -> &[MerkleNode] {
        &self.merkle_branches
    }

    fn prevhash(&self) -> PrevHash {
        self.previous_block_hash.into()
    }

    fn version(&self) -> Version {
        self.version
    }

    fn nbits(&self) -> Nbits {
        self.bits
    }

    fn ntime(&self) -> Ntime {
        self.current_time
    }

    fn height(&self) -> i32 {
        self.height as i32
    }

    fn coinbase_value(&self) -> Option<i64> {
        Some(self.coinbase_value.to_sat() as i64)
    }

    fn create_job(
        self: &Arc<Self>,
        enonce1: &Extranonce,
        enonce2_size: usize,
        address: Option<&Address>,
        job_id: JobId,
        version_mask: Option<Version>,
    ) -> Result<Job<Self>> {
        let address = address.ok_or_else(|| anyhow!("pool mode requires address"))?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system time before UNIX epoch")?
            .as_secs();

        let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address.clone(),
            enonce1.clone(),
            enonce2_size,
            self.height,
            self.coinbase_value,
            self.default_witness_commitment.clone(),
        )
        .with_aux(self.coinbaseaux.clone())
        .with_timestamp(timestamp)
        .with_pool_sig("|parasite|".into())
        .build()
        .context("failed to build coinbase")?;

        Ok(Job {
            job_id,
            coinb1,
            coinb2,
            enonce1: enonce1.clone(),
            version_mask,
            workbase: self.clone(),
        })
    }

    fn clean_jobs(&self, prev: Option<&Self>) -> bool {
        prev.map(|prev| prev.height != self.height).unwrap_or(true)
    }

    fn build_block(&self, job: &Job<Self>, submit: &Submit, header: Header) -> Result<Block> {
        let coinbase_bin = hex::decode(format!(
            "{}{}{}{}",
            job.coinb1, job.enonce1, submit.enonce2, job.coinb2,
        ))
        .context("failed to decode coinbase hex")?;

        let mut cursor = bitcoin::io::Cursor::new(&coinbase_bin);
        let coinbase_tx = Transaction::consensus_decode_from_finite_reader(&mut cursor)
            .context("failed to decode coinbase transaction")?;

        let txdata = std::iter::once(coinbase_tx)
            .chain(self.transactions.iter().map(|tx| tx.transaction.clone()))
            .collect();

        let block = Block { header, txdata };

        if self.height > 16 {
            ensure!(
                block.bip34_block_height().is_ok(),
                "block has invalid BIP34 height encoding"
            );
        }

        Ok(block)
    }
}

impl Workbase for Notify {
    fn merkle_branches(&self) -> &[MerkleNode] {
        &self.merkle_branches
    }

    fn prevhash(&self) -> PrevHash {
        self.prevhash.clone()
    }

    fn version(&self) -> Version {
        self.version
    }

    fn nbits(&self) -> Nbits {
        self.nbits
    }

    fn ntime(&self) -> Ntime {
        self.ntime
    }

    fn height(&self) -> i32 {
        0
    }

    fn coinbase_value(&self) -> Option<i64> {
        None
    }

    fn create_job(
        self: &Arc<Self>,
        enonce1: &Extranonce,
        _enonce2_size: usize,
        _address: Option<&Address>,
        job_id: JobId,
        version_mask: Option<Version>,
    ) -> Result<Job<Self>> {
        Ok(Job {
            job_id,
            coinb1: self.coinb1.clone(),
            coinb2: self.coinb2.clone(),
            enonce1: enonce1.clone(),
            version_mask,
            workbase: self.clone(),
        })
    }

    fn clean_jobs(&self, _prev: Option<&Self>) -> bool {
        self.clean_jobs
    }

    fn upstream_job_id(&self) -> Option<JobId> {
        Some(self.job_id)
    }

    fn build_block(&self, _job: &Job<Self>, _submit: &Submit, _header: Header) -> Result<Block> {
        bail!("proxy mode does not build blocks")
    }
}

#[cfg(test)]
mod tests {
    use {super::*, bitcoin::block};

    fn sample_notify() -> Notify {
        Notify {
            job_id: JobId::new(0xbf),
            prevhash: "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000"
                .parse()
                .unwrap(),
            coinb1: "foo".into(),
            coinb2: "bar".into(),
            merkle_branches: Vec::new(),
            version: Version(block::Version::TWO),
            nbits: "1c2ac4af".parse().unwrap(),
            ntime: "504e86b9".parse().unwrap(),
            clean_jobs: false,
        }
    }

    #[test]
    fn notify_job_ids() {
        let notify = Arc::new(sample_notify());
        let enonce1 = Extranonce::zeros(4);
        let local_id = JobId::new(42);

        let job = notify
            .create_job(&enonce1, 4, None, local_id, None)
            .unwrap();

        assert_eq!(job.job_id, local_id);
        assert_eq!(notify.upstream_job_id(), Some(JobId::new(0xbf)));
    }
}
