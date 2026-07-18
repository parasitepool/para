#![no_main]

use {
    arbitrary::Arbitrary,
    libfuzzer_sys::fuzz_target,
    stratum::{Extranonce, MerkleNode, merkle_root},
};

#[derive(Clone, Debug, Arbitrary)]
struct Input {
    coinb1: String,
    coinb2: String,
    enonce1: Vec<u8>,
    enonce2: Vec<u8>,
    branches: Vec<[u8; 32]>,
}

fuzz_target!(|input: Input| {
    let enonce1 = Extranonce::from_bytes(&input.enonce1);
    let enonce2 = Extranonce::from_bytes(&input.enonce2);

    let branches = input
        .branches
        .iter()
        .copied()
        .map(MerkleNode::from_byte_array)
        .collect::<Vec<MerkleNode>>();

    let result = merkle_root(&input.coinb1, &input.coinb2, &enonce1, &enonce2, &branches);

    if hex::decode(&input.coinb1).is_ok() && hex::decode(&input.coinb2).is_ok() {
        result.unwrap();
    }
});
