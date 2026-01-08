use super::*;

#[derive(Clone, Debug)]
pub(crate) struct Workbase<S> {
    inner: S,
    merkle_branches: Vec<MerkleNode>,
}

impl<S> Workbase<S> {
    pub(crate) fn merkle_branches(&self) -> &[MerkleNode] {
        &self.merkle_branches
    }
}

impl Workbase<BlockTemplate> {
    pub(crate) fn new(template: BlockTemplate) -> Self {
        let merkle_branches =
            stratum::merkle_branches(template.transactions.iter().map(|tx| tx.txid).collect());
        Self {
            inner: template,
            merkle_branches,
        }
    }
    pub(crate) fn template(&self) -> &BlockTemplate {
        &self.inner
    }
}

impl Workbase<Notify> {
    pub(crate) fn new(notify: Notify) -> Self {
        Self {
            merkle_branches: notify.merkle_branches.clone(),
            inner: notify,
        }
    }
    pub(crate) fn notify(&self) -> &Notify {
        &self.inner
    }
    pub(crate) fn clean_jobs(&self) -> bool {
        self.inner.clean_jobs
    }
}
