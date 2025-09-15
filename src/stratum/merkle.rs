use super::*;


/// Stratum uses the the natural big-endian hex encoding of a hash but for some reason Txid and other
/// sha256d::Hash are displayed in little-endian hex in Bitcoin.
#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    DeserializeFromStr,
    SerializeDisplay,
    Display,
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

//impl Serialize for MerkleNode {
//    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
//        let mut bytes = *self.as_byte_array();
//        bytes.reverse();
//        serializer.serialize_str(&hex::encode(bytes))
//    }
//}
//
//impl<'de> Deserialize<'de> for MerkleNode {
//    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
//        let string = String::deserialize(deserializer)?;
//        if string.len() != 64 {
//            return Err(de::Error::custom("merkle node hex must be 64 chars"));
//        }
//        let mut bytes = [0u8; 32];
//        hex::decode_to_slice(&string, &mut bytes).map_err(|e| de::Error::custom(e.to_string()))?;
//        // bytes.reverse();
//        Ok(MerkleNode::from_byte_array(bytes))
//    }
//}

/// Display as it would on the wire
// impl fmt::Display for MerkleNode {
// wire = little-endian hex
// fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
// let mut b = *self.0.as_byte_array();
// b.reverse();
// write!(f, "{}", hex::encode(b))
// }
// }

impl FromStr for MerkleNode {
    type Err = anyhow::Error;

    // parse wire LE hex
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ensure!(s.len() == 64, "merkle node hex must be 64 chars");
        let mut b = [0u8; 32];
        hex::decode_to_slice(s, &mut b)?;
        // b.reverse(); // LE -> internal
        Ok(MerkleNode(sha256d::Hash::from_byte_array(b)))
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

    #[track_caller]
    fn case(wire_hex: &str, display_hex: &str) {
        dbg!(&wire_hex);
        let from_wire: MerkleNode = serde_json::from_str(&format!("\"{wire_hex}\"")).unwrap();

        let raw_hash: sha256d::Hash = serde_json::from_str(&format!("\"{wire_hex}\"")).unwrap();

        dbg!(&from_wire);
        dbg!(&raw_hash);

        assert_eq!(
            from_wire.to_string(),
            wire_hex,
            "Display renders wire format"
        );

        // Underlying raw hash (big-endian hex) must match be_hex
        // let raw_from_wire: sha256d::Hash = ;
        assert_eq!(
            from_wire.to_raw_hash(),
            sha256d::Hash::from_str(display_hex).unwrap()
        );

        // Construct from raw BE hash and verify equivalences
        // let raw = sha256d::Hash::from_str(be_hex).unwrap();
        // let merkle_node = MerkleNode::from_raw_hash(raw);
        // assert_eq!(node_from_raw, from_wire);
        // assert_eq!(sha256d::Hash::from(node_from_raw), raw);

        // JSON round-trip preserves wire form
        // let serialized = serde_json::to_string(&merkle_node).unwrap();
        // assert_eq!(serialized, format!("\"{wire_hex}\""));

        // let round_trip = serde_json::from_str::<MerkleNode>(&serialized).unwrap();
        // assert_eq!(round_trip, merkle_node);
    }

    fn extranonce1() -> Extranonce {
        "abcd1234".parse().unwrap()
    }

    fn extranonce2() -> Extranonce {
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
    fn roundtrip_all_zero() {
        let z = "0".repeat(64);
        case(&z, &z);
    }

    #[test]
    fn roundtrip_sequential_bytes() {
        let display_hex = "adc3a8d948de28cf8747dfafa39768770e2dc56fcd08bd5e21e2b943345ef6c0";
        let wire_hex = "c0f65e3443b9e2215ebd08cd6fc52d0e776897a3afdf4787cf28de48d9a8c3ad";
        case(wire_hex, display_hex);
    }

    #[test]
    fn roundtrip_all_ff() {
        let f = "f".repeat(64);
        case(&f, &f);
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
        let extranonce1 = extranonce1();
        let extranonce2 = extranonce2();

        let want = {
            let bin = hex::decode(format!("{coinb1}{extranonce1}{extranonce2}{coinb2}")).unwrap();
            MerkleNode::from_raw_hash(sha256d::Hash::hash(&bin))
        };

        let got = merkle_root(coinb1, coinb2, &extranonce1, &extranonce2, &[]).unwrap();

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
        let extranonce1 = extranonce1();
        let extranonce2 = extranonce2();

        let root = merkle_root(coinb1, coinb2, &extranonce1, &extranonce2, &branches).unwrap();

        let branch_0 = {
            let bin = hex::decode(format!("{coinb1}{extranonce1}{extranonce2}{coinb2}")).unwrap();
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
        let extranonce1 = extranonce1();
        let extranonce2 = extranonce2();

        let root = merkle_root(coinb1, coinb2, &extranonce1, &extranonce2, &branches).unwrap();

        let branch_0 = {
            let bin = hex::decode(format!("{coinb1}{extranonce1}{extranonce2}{coinb2}")).unwrap();
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
        let extranonce1 = extranonce1();
        let extranonce2 = extranonce2();

        let root = merkle_root(coinb1, coinb2, &extranonce1, &extranonce2, &branches).unwrap();

        let branch_0 = {
            let bin = hex::decode(format!("{coinb1}{extranonce1}{extranonce2}{coinb2}")).unwrap();
            let coinbase_txid = MerkleNode::from_raw_hash(sha256d::Hash::hash(&bin));

            hash(coinbase_txid.into(), txid(1).into())
        };

        assert_eq!(
            root,
            MerkleNode::from_raw_hash(hash(hash(branch_0, branch_1.into()), branch_2.into()))
        );
    }
}
