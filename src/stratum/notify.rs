use super::*;

#[derive(Debug, PartialEq)]
pub struct Notify {
    pub job_id: String,
    pub prevhash: PrevHash,
    pub coinb1: String,
    pub coinb2: String,
    pub merkle_branches: Vec<TxMerkleNode>,
    pub version: Version,
    pub nbits: Nbits,
    pub ntime: Ntime,
    pub clean_jobs: bool,
}

impl Serialize for Notify {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(9))?;
        seq.serialize_element(&self.job_id)?;
        seq.serialize_element(&self.prevhash)?;
        seq.serialize_element(&self.coinb1)?;
        seq.serialize_element(&self.coinb2)?;
        seq.serialize_element(&self.merkle_branches)?;
        seq.serialize_element(&self.version)?;
        seq.serialize_element(&self.nbits)?;
        seq.serialize_element(&self.ntime)?;
        seq.serialize_element(&self.clean_jobs)?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Notify {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (job_id, prevhash, coinb1, coinb2, merkle_branches, version, nbits, ntime, clean_jobs) =
            <(
                String,
                PrevHash,
                String,
                String,
                Vec<TxMerkleNode>,
                Version,
                Nbits,
                Ntime,
                bool,
            )>::deserialize(deserializer)?;

        Ok(Notify {
            job_id,
            prevhash,
            coinb1,
            coinb2,
            merkle_branches,
            version,
            nbits,
            ntime,
            clean_jobs,
        })
    }
}
