use super::*;

#[derive(Clone, Debug)]
pub(crate) struct Workbase {
    template: BlockTemplate,
    merkle_branches: Vec<MerkleNode>,
}

impl Workbase {
    pub(crate) fn new(template: BlockTemplate) -> Self {
        let merkle_branches =
            stratum::merkle_branches(template.transactions.iter().map(|tx| tx.txid).collect());

        Self {
            template,
            merkle_branches,
        }
    }

    pub(crate) fn template(&self) -> &BlockTemplate {
        &self.template
    }

    pub(crate) fn merkle_branches(&self) -> &[MerkleNode] {
        &self.merkle_branches
    }
}
