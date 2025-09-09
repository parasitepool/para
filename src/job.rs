use super::*;

#[derive(Debug)]
pub(crate) struct Job {
    pub(crate) coinb1: String,
    pub(crate) coinb2: String,
    pub(crate) extranonce1: Extranonce,
    pub(crate) gbt: GetBlockTemplateResult,
    pub(crate) job_id: String,
    pub(crate) merkle_branches: Vec<TxMerkleNode>,
    pub(crate) version_mask: Option<Version>,
}

impl Job {
    pub(crate) fn new(
        address: Address,
        extranonce1: Extranonce,
        version_mask: Option<Version>,
        gbt: GetBlockTemplateResult,
    ) -> Result<Self> {
        let job_id = "deadbeef".to_string();

        let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address,
            extranonce1.clone(),
            EXTRANONCE2_SIZE,
            gbt.height,
            gbt.coinbase_value,
            gbt.default_witness_commitment.clone(),
        )
        .with_pool_sig("|parasite|".into())
        .build()?;

        let merkle_branches = stratum::merkle_branches(
            gbt.transactions
                .clone()
                .into_iter()
                .map(|r| r.txid)
                .collect(),
        );

        Ok(Self {
            coinb1,
            coinb2,
            extranonce1,
            gbt,
            job_id,
            merkle_branches,
            version_mask,
        })
    }

    pub(crate) fn nbits(&self) -> Result<Nbits> {
        Nbits::from_str(&hex::encode(&self.gbt.bits))
    }

    pub(crate) fn prevhash(&self) -> PrevHash {
        PrevHash::from(self.gbt.previous_block_hash)
    }

    pub(crate) fn version(&self) -> Version {
        Version(block::Version::from_consensus(
            self.gbt.version.try_into().unwrap(),
        ))
    }

    pub(crate) fn notify(&self) -> Result<Notify> {
        Ok(Notify {
            job_id: self.job_id.clone(),
            prevhash: self.prevhash(),
            coinb1: self.coinb1.clone(),
            coinb2: self.coinb2.clone(),
            merkle_branches: self.merkle_branches.clone(),
            version: self.version(),
            nbits: self.nbits()?,
            ntime: Ntime::try_from(self.gbt.current_time).expect("fits until ~2106"),
            clean_jobs: true,
        })
    }
}
