use super::*;

#[derive(Debug)]
pub(crate) struct Job {
    pub(crate) coinb1: String,
    pub(crate) coinb2: String,
    pub(crate) enonce1: Extranonce,
    pub(crate) workbase: Arc<Workbase>,
    pub(crate) job_id: JobId,
    pub(crate) version_mask: Option<Version>,
}

impl Job {
    pub(crate) fn new(
        address: Address,
        enonce1: Extranonce,
        enonce2_size: usize,
        version_mask: Option<Version>,
        workbase: Arc<Workbase>,
        job_id: JobId,
    ) -> Result<Self> {
        let template = workbase.template();
        let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address.clone(),
            enonce1.clone(),
            enonce2_size,
            template.height,
            template.coinbase_value,
            template.default_witness_commitment.clone(),
        )
        .with_aux(template.coinbaseaux.clone())
        .with_timestamp(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
        .with_pool_sig("|parasite|".into())
        .build()?;

        Ok(Self {
            coinb1,
            coinb2,
            enonce1,
            workbase,
            job_id,
            version_mask,
        })
    }

    pub(crate) fn nbits(&self) -> Nbits {
        self.workbase.template().bits
    }

    pub(crate) fn prevhash(&self) -> PrevHash {
        PrevHash::from(self.workbase.template().previous_block_hash)
    }

    pub(crate) fn version(&self) -> Version {
        self.workbase.template().version
    }

    pub(crate) fn notify(&self, clean_jobs: bool) -> Result<Notify> {
        Ok(Notify {
            job_id: self.job_id,
            prevhash: self.prevhash(),
            coinb1: self.coinb1.clone(),
            coinb2: self.coinb2.clone(),
            merkle_branches: self.workbase.merkle_branches().to_vec(),
            version: self.version(),
            nbits: self.nbits(),
            ntime: self.workbase.template().current_time,
            clean_jobs,
        })
    }
}
