use super::*;

#[derive(Debug)]
pub(crate) struct Job<W: Workbase> {
    pub(crate) job_id: JobId,
    pub(crate) upstream_job_id: JobId,
    pub(crate) coinb1: String,
    pub(crate) coinb2: String,
    pub(crate) enonce1: Extranonce,
    pub(crate) version_mask: Option<Version>,
    pub(crate) workbase: Arc<W>,
}

impl<W: Workbase> Job<W> {
    pub(crate) fn prevhash(&self) -> PrevHash {
        self.workbase.prevhash()
    }

    pub(crate) fn merkle_branches(&self) -> &[MerkleNode] {
        self.workbase.merkle_branches()
    }

    pub(crate) fn version(&self) -> Version {
        self.workbase.version()
    }

    pub(crate) fn nbits(&self) -> Nbits {
        self.workbase.nbits()
    }

    pub(crate) fn ntime(&self) -> Ntime {
        self.workbase.ntime()
    }

    pub(crate) fn notify(&self, clean_jobs: bool) -> Result<Notify> {
        Ok(Notify {
            job_id: self.job_id,
            prevhash: self.prevhash(),
            coinb1: self.coinb1.clone(),
            coinb2: self.coinb2.clone(),
            merkle_branches: self.merkle_branches().to_vec(),
            version: self.version(),
            nbits: self.nbits(),
            ntime: self.ntime(),
            clean_jobs,
        })
    }
}
