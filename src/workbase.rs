use super::*;

#[derive(Clone, Debug)]
pub struct Workbase {
    template: BlockTemplate,
    merkle_branches: Vec<MerkleNode>,
}

impl Workbase {
    pub fn new(template: BlockTemplate) -> Self {
        let merkle_branches =
            stratum::merkle_branches(template.transactions.iter().map(|tx| tx.txid).collect());

        Self {
            template,
            merkle_branches,
        }
    }

    pub fn template(&self) -> &BlockTemplate {
        &self.template
    }

    pub fn merkle_branches(&self) -> &[MerkleNode] {
        &self.merkle_branches
    }
}
