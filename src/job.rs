use super::*;

#[derive(Debug)]
pub(crate) struct Job {
    pub(crate) coinb1: String,
    pub(crate) coinb2: String,
    pub(crate) extranonce1: Extranonce,
    pub(crate) template: Arc<BlockTemplate>,
    pub(crate) job_id: String,
    pub(crate) merkle_branches: Vec<MerkleNode>,
    pub(crate) version_mask: Option<Version>,
}

impl Job {
    pub(crate) fn new(
        address: Address,
        extranonce1: Extranonce,
        version_mask: Option<Version>,
        template: Arc<BlockTemplate>,
        job_id: String,
    ) -> Result<Self> {
        let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address,
            extranonce1.clone(),
            EXTRANONCE2_SIZE,
            template.height,
            template.coinbase_value,
            template.default_witness_commitment.clone(),
        )
        .with_aux(template.coinbaseaux.clone())
        .with_timestamp(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
        .with_pool_sig("|parasite|".into())
        .build()?;

        let merkle_branches =
            stratum::merkle_branches(template.transactions.iter().map(|tx| tx.txid).collect());

        Ok(Self {
            coinb1,
            coinb2,
            extranonce1,
            template,
            job_id,
            merkle_branches,
            version_mask,
        })
    }

    pub(crate) fn nbits(&self) -> Nbits {
        self.template.bits
    }

    pub(crate) fn prevhash(&self) -> PrevHash {
        PrevHash::from(self.template.previous_block_hash)
    }

    pub(crate) fn version(&self) -> Version {
        self.template.version
    }

    pub(crate) fn notify(&self) -> Result<Notify> {
        Ok(Notify {
            job_id: self.job_id.clone(),
            prevhash: self.prevhash(),
            coinb1: self.coinb1.clone(),
            coinb2: self.coinb2.clone(),
            merkle_branches: self.merkle_branches.clone(),
            version: self.version(),
            nbits: self.nbits(),
            ntime: self.template.current_time,
            clean_jobs: true,
        })
    }
}
