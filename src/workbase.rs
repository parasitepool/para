use super::*;

/// Workbase combines a BlockTemplate with its pre-computed merkle branches.
/// This ensures that the template and merkle branches are always in sync and
/// computed exactly once per template.
#[derive(Clone, Debug)]
pub struct Workbase {
    template: BlockTemplate,
    merkle_branches: Vec<MerkleNode>,
}

impl Workbase {
    /// Creates a new Workbase from a BlockTemplate, automatically computing
    /// the merkle branches from the template's transactions.
    pub fn new(template: BlockTemplate) -> Self {
        let merkle_branches = stratum::merkle_branches(
            template.transactions.iter().map(|tx| tx.txid).collect(),
        );

        Self {
            template,
            merkle_branches,
        }
    }

    /// Returns a reference to the block template.
    pub fn template(&self) -> &BlockTemplate {
        &self.template
    }

    /// Returns a reference to the merkle branches.
    pub fn merkle_branches(&self) -> &[MerkleNode] {
        &self.merkle_branches
    }
}

