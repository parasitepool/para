use super::*;

pub fn merkle_root(
    coinb1: &str,
    coinb2: &str,
    extranonce1: &str,
    extranonce2: &str,
    merkle_branches: &[TxMerkleNode],
) -> Result<TxMerkleNode> {
    let coinbase_bin = hex::decode(format!("{coinb1}{extranonce1}{extranonce2}{coinb2}"))?;
    let coinbase_hash = sha256d::Hash::hash(&coinbase_bin);

    let mut merkle_root = coinbase_hash;
    for branch in merkle_branches {
        let mut concat = Vec::with_capacity(64);
        concat.extend_from_slice(&merkle_root[..]);
        concat.extend_from_slice(branch.as_byte_array());
        merkle_root = sha256d::Hash::hash(&concat);
    }

    Ok(TxMerkleNode::from_raw_hash(merkle_root))
}

pub fn merkle_branches(non_coinbase_txids: Vec<Txid>) -> Vec<TxMerkleNode> {
    let total_txs = non_coinbase_txids.len() + 1;

    if total_txs <= 1 {
        return vec![];
    }

    let mut level: Vec<TxMerkleNode> = vec![TxMerkleNode::all_zeros()];

    level.extend(
        non_coinbase_txids
            .iter()
            .map(|id| TxMerkleNode::from_raw_hash(id.to_raw_hash())),
    );

    let mut branches: Vec<TxMerkleNode> = Vec::new();
    let mut coinbase_idx = 0usize;

    while level.len() > 1 {
        let sibling_idx = coinbase_idx ^ 1;

        let sibling = if sibling_idx < level.len() {
            level[sibling_idx]
        } else {
            level[coinbase_idx]
        };

        branches.push(sibling);

        let mut next_level = Vec::with_capacity(level.len() / 2 + 1);
        let mut i = 0;
        while i < level.len() {
            let hash1 = level[i];
            let hash2 = if i + 1 < level.len() {
                level[i + 1]
            } else {
                hash1
            };

            let mut engine = <TxMerkleNode as Hash>::engine();

            hash1
                .consensus_encode(&mut engine)
                .expect("in-memory writers don't error");

            hash2
                .consensus_encode(&mut engine)
                .expect("in-memory writers don't error");

            next_level.push(TxMerkleNode::from_engine(engine));

            i += 2;
        }

        level = next_level;
        coinbase_idx /= 2;
    }

    branches
}
