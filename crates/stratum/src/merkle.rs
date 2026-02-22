use super::*;

/// Stratum uses the the natural big-endian hex encoding of a hash but for some reason
/// all double sha256d::Hash are displayed in little-endian hex in Bitcoin. To ensure correct
/// serialization/deserialization and display this wrapper type was created.
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, DeserializeFromStr, SerializeDisplay,
)]
pub struct MerkleNode(sha256d::Hash);

impl MerkleNode {
    pub fn as_byte_array(&self) -> &[u8; 32] {
        self.0.as_byte_array()
    }

    pub fn from_byte_array(b: [u8; 32]) -> Self {
        Self(sha256d::Hash::from_byte_array(b))
    }

    pub fn from_raw_hash(h: sha256d::Hash) -> Self {
        Self(h)
    }

    pub fn to_raw_hash(self) -> sha256d::Hash {
        self.0
    }

    pub fn all_zeros() -> Self {
        Self(sha256d::Hash::all_zeros())
    }
}

impl FromStr for MerkleNode {
    type Err = InternalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 {
            return Err(InternalError::InvalidLength {
                expected: 64,
                actual: s.len(),
            });
        }
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(s, &mut bytes).context(error::HexParseSnafu)?;
        Ok(MerkleNode(sha256d::Hash::from_byte_array(bytes)))
    }
}

/// Double-SHA256 hashes in Bitcoin are usually displayed in little-endian format. Here we
/// specifically do not do that and instead display it in natural big-endian format.
impl fmt::Display for MerkleNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(*self.0.as_byte_array()))
    }
}

impl From<sha256d::Hash> for MerkleNode {
    fn from(h: sha256d::Hash) -> Self {
        Self(h)
    }
}

impl From<MerkleNode> for sha256d::Hash {
    fn from(n: MerkleNode) -> Self {
        n.0
    }
}

impl From<MerkleNode> for TxMerkleNode {
    fn from(n: MerkleNode) -> Self {
        n.0.into()
    }
}

impl From<bitcoin::Txid> for MerkleNode {
    fn from(txid: bitcoin::Txid) -> Self {
        Self::from_byte_array(txid.to_byte_array())
    }
}

/// Calculates the merkle root by reassembling the coinbase transaction and rebuilding
/// the merkle tree from the merkle branches. This is definitely a target for some
/// optimization in the future.
pub fn merkle_root(
    coinb1: &str,
    coinb2: &str,
    enonce1: &Extranonce,
    enonce2: &Extranonce,
    merkle_branches: &[MerkleNode],
) -> Result<MerkleNode, InternalError> {
    let coinbase_bin =
        hex::decode(format!("{coinb1}{enonce1}{enonce2}{coinb2}")).context(error::HexParseSnafu)?;
    let coinbase_hash = sha256d::Hash::hash(&coinbase_bin);

    let mut merkle_root = coinbase_hash;
    for branch in merkle_branches {
        let mut concat = Vec::with_capacity(64);
        concat.extend_from_slice(&merkle_root[..]);
        concat.extend_from_slice(branch.as_byte_array());
        merkle_root = sha256d::Hash::hash(&concat);
    }

    Ok(MerkleNode::from_raw_hash(merkle_root))
}

/// Constructs the merkle branches from all non-coinbase transactions that should be included in
/// the block.
pub fn merkle_branches(non_coinbase_txids: Vec<Txid>) -> Vec<MerkleNode> {
    if non_coinbase_txids.is_empty() {
        return Vec::new();
    }

    let mut level = vec![sha256d::Hash::all_zeros()];

    level.extend(non_coinbase_txids.iter().map(|id| id.to_raw_hash()));

    let mut branches: Vec<MerkleNode> = Vec::new();
    let mut coinbase_index = 0;

    while level.len() > 1 {
        // XOR to get sibling (can be right or left sibling)
        let sibling_index = coinbase_index ^ 1;

        let sibling = if sibling_index < level.len() {
            level[sibling_index]
        } else {
            level[coinbase_index]
        };

        branches.push(sibling.into());

        let mut next_level = Vec::with_capacity(level.len() / 2 + 1);
        let mut i = 0;
        while i < level.len() {
            let hash1 = level[i];
            let hash2 = if i + 1 < level.len() {
                level[i + 1]
            } else {
                hash1
            };

            let mut engine = <sha256d::Hash>::engine();

            hash1
                .consensus_encode(&mut engine)
                .expect("in-memory writer shouldn't error");

            hash2
                .consensus_encode(&mut engine)
                .expect("in-memory writer shouldn't error");

            next_level.push(sha256d::Hash::from_engine(engine));

            i += 2;
        }

        level = next_level;
        coinbase_index /= 2;
    }

    branches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enonce1() -> Extranonce {
        "abcd1234".parse().unwrap()
    }

    fn enonce2() -> Extranonce {
        "0011223344556677".parse().unwrap()
    }

    fn txid(n: u32) -> Txid {
        let hex = format!("{n:x}");

        if hex.is_empty() || hex.len() > 1 {
            panic!();
        }

        hex.repeat(64).parse().unwrap()
    }

    fn hash(a: sha256d::Hash, b: sha256d::Hash) -> sha256d::Hash {
        let mut concat = Vec::with_capacity(64);
        concat.extend_from_slice(&a[..]);
        concat.extend_from_slice(&b[..]);
        sha256d::Hash::hash(&concat)
    }

    #[test]
    fn roundtrip_merkle_node() {
        let bitcoin_hex = "adc3a8d948de28cf8747dfafa39768770e2dc56fcd08bd5e21e2b943345ef6c0";
        let stratum_hex = "c0f65e3443b9e2215ebd08cd6fc52d0e776897a3afdf4787cf28de48d9a8c3ad";

        let hash = sha256d::Hash::from_str(bitcoin_hex).unwrap();
        let merkle_node = MerkleNode::from_str(stratum_hex).unwrap();

        assert_eq!(
            hash.to_byte_array(),
            merkle_node.to_raw_hash().to_byte_array()
        );

        assert_eq!(hash.to_string(), bitcoin_hex);
        assert_eq!(merkle_node.to_string(), stratum_hex);

        let raw_hash_from_wire: sha256d::Hash =
            serde_json::from_str(&format!("\"{bitcoin_hex}\"")).unwrap();

        let merkle_from_wire: MerkleNode =
            serde_json::from_str(&format!("\"{stratum_hex}\"")).unwrap();

        assert_eq!(
            raw_hash_from_wire.to_byte_array(),
            merkle_from_wire.to_raw_hash().to_byte_array()
        );

        let serialized = serde_json::to_string(&merkle_node).unwrap();
        assert_eq!(serialized, format!("\"{stratum_hex}\""));

        let round_trip = serde_json::from_str::<MerkleNode>(&serialized).unwrap();
        assert_eq!(round_trip, merkle_node);
    }

    #[test]
    fn empty_merkle_branches_when_only_coinbase() {
        let branches = merkle_branches(Vec::new());
        assert!(branches.is_empty());
    }

    #[test]
    fn single_txid_branch_is_the_merkle_node() {
        let branches = merkle_branches(vec![txid(1)]);
        assert_eq!(branches.len(), 1);
        assert_eq!(
            branches[0],
            MerkleNode::from_raw_hash(txid(1).to_raw_hash())
        );
        assert_eq!(branches[0], MerkleNode::from(txid(1)));
    }

    #[test]
    fn merkle_root_no_branches_equals_hash_of_coinbase() {
        let coinb1 = "aa";
        let coinb2 = "dd";
        let enonce1 = enonce1();
        let enonce2 = enonce2();

        let want = {
            let bin = hex::decode(format!("{coinb1}{enonce1}{enonce2}{coinb2}")).unwrap();
            MerkleNode::from_raw_hash(sha256d::Hash::hash(&bin))
        };

        let got = merkle_root(coinb1, coinb2, &enonce1, &enonce2, &[]).unwrap();

        assert_eq!(want, got);
    }

    #[test]
    fn two_level_tree() {
        // Branches: [ t1 , H(t2||t2) ]
        let branches = merkle_branches(vec![txid(1), txid(2)]);

        assert_eq!(branches.len(), 2);
        assert_eq!(branches[0], MerkleNode::from(txid(1)));

        let branch_1 = MerkleNode::from_raw_hash(hash(txid(2).into(), txid(2).into()));

        assert_eq!(branches[1], branch_1);

        // Leaves: [ coinbase , t1 , t2 ] → root = H( H(cb||t1) || H(t2||t2) )
        let coinb1 = "aa";
        let coinb2 = "dd";
        let enonce1 = enonce1();
        let enonce2 = enonce2();

        let root = merkle_root(coinb1, coinb2, &enonce1, &enonce2, &branches).unwrap();

        let branch_0 = {
            let bin = hex::decode(format!("{coinb1}{enonce1}{enonce2}{coinb2}")).unwrap();
            let coinbase_txid = MerkleNode::from_raw_hash(sha256d::Hash::hash(&bin));

            hash(coinbase_txid.into(), txid(1).into())
        };

        assert_eq!(
            root,
            MerkleNode::from_raw_hash(hash(branch_0, branch_1.into()))
        );
    }

    #[test]
    fn two_level_tree_with_three_transactions() {
        // Branches: [ t1 , H(t2||t3) ]
        let branches = merkle_branches(vec![txid(1), txid(2), txid(3)]);

        assert_eq!(branches.len(), 2);
        assert_eq!(branches[0], MerkleNode::from(txid(1)));

        let branch_1 = MerkleNode::from_raw_hash(hash(txid(2).into(), txid(3).into()));

        assert_eq!(branches[1], branch_1);

        // Leaves: [ coinbase , t1 , t2, t3 ] → root = H( H(cb||t1) || H(t2||t3) )
        let coinb1 = "aa";
        let coinb2 = "dd";
        let enonce1 = enonce1();
        let enonce2 = enonce2();

        let root = merkle_root(coinb1, coinb2, &enonce1, &enonce2, &branches).unwrap();

        let branch_0 = {
            let bin = hex::decode(format!("{coinb1}{enonce1}{enonce2}{coinb2}")).unwrap();
            let coinbase_txid = MerkleNode::from_raw_hash(sha256d::Hash::hash(&bin));

            hash(coinbase_txid.into(), txid(1).into())
        };

        assert_eq!(
            root,
            MerkleNode::from_raw_hash(hash(branch_0, branch_1.into()))
        );
    }

    #[test]
    fn three_level_tree() {
        // Branches: [ t1, H(t2||t3), H(H(t4||t5) || H(t4||t5)) ]
        let branches = merkle_branches(vec![txid(1), txid(2), txid(3), txid(4), txid(5)]);

        assert_eq!(branches.len(), 3);
        assert_eq!(branches[0], MerkleNode::from(txid(1)));

        let branch_1 = MerkleNode::from(hash(txid(2).into(), txid(3).into()));
        let branch_2 = {
            MerkleNode::from(hash(
                hash(txid(4).into(), txid(5).into()),
                hash(txid(4).into(), txid(5).into()),
            ))
        };

        assert_eq!(branches[1], branch_1);
        assert_eq!(branches[2], branch_2);

        // Leaves: [ coinbase , t1 , t2, t3, t4, t5 ] → root = H (H( H(cb||t1) || H(t2||t3) ) H (H(t4||t5) || H(t4||t5)))
        let coinb1 = "aa";
        let coinb2 = "dd";
        let enonce1 = enonce1();
        let enonce2 = enonce2();

        let root = merkle_root(coinb1, coinb2, &enonce1, &enonce2, &branches).unwrap();

        let branch_0 = {
            let bin = hex::decode(format!("{coinb1}{enonce1}{enonce2}{coinb2}")).unwrap();
            let coinbase_txid = MerkleNode::from_raw_hash(sha256d::Hash::hash(&bin));

            hash(coinbase_txid.into(), txid(1).into())
        };

        assert_eq!(
            root,
            MerkleNode::from_raw_hash(hash(hash(branch_0, branch_1.into()), branch_2.into()))
        );
    }
}
