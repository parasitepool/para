use super::*;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

impl Serialize for MerkleNode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut bytes = *self.as_byte_array();
        bytes.reverse();
        serializer.serialize_str(&hex::encode(bytes))
    }
}

impl<'de> Deserialize<'de> for MerkleNode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let string = String::deserialize(deserializer)?;
        if string.len() != 64 {
            return Err(de::Error::custom("merkle node hex must be 64 chars"));
        }
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(&string, &mut bytes).map_err(|e| de::Error::custom(e.to_string()))?;
        bytes.reverse();
        Ok(MerkleNode::from_byte_array(bytes))
    }
}

impl fmt::Display for MerkleNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut b = *self.as_byte_array();
        b.reverse();
        write!(f, "{}", hex::encode(b))
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
/// the merkle tree from the partial merkle branches. This is definitely a target for some
/// optimization in the future.
pub fn merkle_root(
    coinb1: &str,
    coinb2: &str,
    extranonce1: &Extranonce,
    extranonce2: &Extranonce,
    merkle_branches: &[MerkleNode],
) -> Result<MerkleNode> {
    let coinbase_bin = hex::decode(format!("{coinb1}{extranonce1}{extranonce2}{coinb2}"))?;
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

    fn extranonce1() -> Extranonce {
        "abcd1234".parse().unwrap()
    }

    fn extranonce2() -> Extranonce {
        "0011223344556677".parse().unwrap()
    }

    pub(crate) fn txid(n: u32) -> Txid {
        let hex = format!("{n:x}");

        if hex.is_empty() || hex.len() > 1 {
            panic!();
        }

        hex.repeat(64).parse().unwrap()
    }

    #[test]
    fn branches_empty_when_only_coinbase() {
        let v = merkle_branches(Vec::new());
        assert!(v.is_empty());
    }

    #[test]
    fn single_txid_branch_is_the_txid_node() {
        let t1 = txid(0x11);
        let v = merkle_branches(vec![t1]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0], MerkleNode::from_raw_hash(t1.to_raw_hash()));
    }

    #[test]
    fn merkle_root_no_branches_equals_hash_of_coinbase_bytes() {
        // coinbase only → root is H(coinbase)
        let coinb1 = "aa";
        let coinb2 = "dd";
        let x1 = extranonce1();
        let x2 = extranonce2();

        let want = {
            let bin = hex::decode(format!("{coinb1}{x1}{x2}{coinb2}")).unwrap();
            sha256d::Hash::hash(&bin)
        };

        let got = merkle_root(coinb1, coinb2, &x1, &x2, &[]).unwrap();
        assert_eq!(got, MerkleNode::from_raw_hash(want));
    }

    #[test]
    fn merkle_root_matches_manual_two_level_tree() {
        // Leaves: [ coinbase , t1 , t2 ] → root = H( H(cb||t1) || H(t2||t2) )
        let coinb1 = "aa";
        let coinb2 = "dd";
        let x1 = extranonce1();
        let x2 = extranonce2();

        let t1 = txid(0x11);
        let t2 = txid(0x22);

        // Branches for the coinbase should be: [ t1 , H(t2||t2) ]
        let branches = merkle_branches(vec![t1, t2]);

        todo!()
    }
}
