use super::*;

#[derive(Clone, Debug)]
pub(crate) struct Workbase<S> {
    source: S,
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
            source: template,
            merkle_branches,
        }
    }
    pub(crate) fn template(&self) -> &BlockTemplate {
        &self.source
    }
}

impl Workbase<Notify> {
    #[allow(dead_code)]
    pub(crate) fn new(notify: Notify) -> Self {
        Self {
            merkle_branches: notify.merkle_branches.clone(),
            source: notify,
        }
    }
    pub(crate) fn notify(&self) -> &Notify {
        &self.source
    }
}
