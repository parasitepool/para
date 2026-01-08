use super::*;

#[derive(Debug)]
pub(crate) struct Job<S: Source> {
    pub(crate) job_id: JobId,
    pub(crate) prevhash: PrevHash,
    pub(crate) coinb1: String,
    pub(crate) coinb2: String,
    pub(crate) merkle_branches: Vec<MerkleNode>,
    pub(crate) version: Version,
    pub(crate) nbits: Nbits,
    pub(crate) ntime: Ntime,
    pub(crate) enonce1: Extranonce,
    pub(crate) version_mask: Option<Version>,
    pub(crate) workbase: Arc<S>,
}

impl<S: Source> Job<S> {
    pub(crate) fn notify(&self, clean_jobs: bool) -> Result<Notify> {
        Ok(Notify {
            job_id: self.job_id,
            prevhash: self.prevhash.clone(),
            coinb1: self.coinb1.clone(),
            coinb2: self.coinb2.clone(),
            merkle_branches: self.merkle_branches.clone(),
            version: self.version,
            nbits: self.nbits,
            ntime: self.ntime,
            clean_jobs,
        })
    }
}
