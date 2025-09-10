use super::*;

#[derive(Clone)]
pub struct CoinbaseBuilder {
    address: Address,
    aux: BTreeMap<String, String>,
    extranonce1: Extranonce,
    extranonce2_size: usize,
    height: u64,
    pool_sig: Option<String>,
    timestamp: Option<u64>,
    value: Amount,
    witness_commitment: ScriptBuf,
}

impl CoinbaseBuilder {
    const MAX_COINBASE_SCRIPT_SIG_SIZE: usize = 100;

    pub fn new(
        address: Address,
        extranonce1: Extranonce,
        extranonce2_size: usize,
        height: u64,
        value: Amount,
        witness_commitment: ScriptBuf,
    ) -> Self {
        Self {
            address,
            aux: BTreeMap::new(),
            extranonce1,
            extranonce2_size,
            height,
            value,
            witness_commitment,
            timestamp: None,
            pool_sig: None,
        }
    }

    pub fn with_aux(mut self, aux: BTreeMap<String, String>) -> Self {
        self.aux = aux;
        self
    }

    pub fn with_timestamp(mut self, timestamp: u64) -> Self {
        self.timestamp = Some(timestamp);
        self
    }

    pub fn with_pool_sig(mut self, pool_sig: String) -> Self {
        self.pool_sig = Some(pool_sig);
        self
    }

    pub fn build(self) -> Result<(Transaction, String, String)> {
        let mut buf: Vec<u8> = Vec::with_capacity(Self::MAX_COINBASE_SCRIPT_SIG_SIZE);

        // BIP34 encode block height
        let mut minimally_encoded_serialized_cscript = [0u8; 8];
        let len = write_scriptint(
            &mut minimally_encoded_serialized_cscript,
            self.height.try_into().expect("height should always fit"),
        );
        // byte length should be fine for the next 150 years
        buf.push(len as u8);
        buf.extend_from_slice(&minimally_encoded_serialized_cscript[..len]);

        for (_, value) in self.aux.into_iter() {
            buf.extend_from_slice(hex::decode(value)?.as_slice());
        }

        let script_prefix_size = buf.len();

        buf.extend_from_slice(self.extranonce1.as_bytes());
        buf.extend_from_slice(vec![0u8; self.extranonce2_size].as_slice());

        if let Some(sig) = self.pool_sig {
            buf.extend_from_slice(sig.as_bytes())
        }

        if let Some(ts) = self.timestamp {
            buf.extend_from_slice(&ts.to_le_bytes());
        }

        buf.extend_from_slice(&[0x7c, 0x70, 0x61, 0x72, 0x61, 0x7c]);

        let script_sig = ScriptBuf::from_bytes(buf);
        let script_sig_size = script_sig.len();

        ensure!(
            script_sig_size <= Self::MAX_COINBASE_SCRIPT_SIG_SIZE,
            "Script sig too large is {script_sig_size} bytes (max {})",
            Self::MAX_COINBASE_SCRIPT_SIG_SIZE
        );

        let coinbase = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig,
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![
                TxOut {
                    value: self.value,
                    script_pubkey: self.address.script_pubkey(),
                },
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: self.witness_commitment,
                },
            ],
        };

        let total_extranonce_size = self.extranonce1.len() + self.extranonce2_size;

        // offset = size of tx version
        //  + size of #inputs
        //  + size of coinbase outpoint
        //  + size of scriptSig length
        //  + size of everything before extranonce1 + extranonce2
        let offset = 4
            + VarInt(coinbase.input.len().try_into().unwrap()).size()
            + 36
            + VarInt(script_sig_size.try_into().unwrap()).size()
            + script_prefix_size;

        let bin = consensus::serialize(&coinbase);
        let coinb1 = hex::encode(&bin[..offset]);
        let coinb2 = hex::encode(&bin[offset + total_extranonce_size..]);

        Ok((coinbase, coinb1, coinb2))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, bitcoin::address::NetworkUnchecked,
        pretty_assertions::assert_eq as pretty_assert_eq,
    };

    fn address() -> Address {
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    #[test]
    fn exceed_script_size_limit() {
        let result = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            0,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .with_pool_sig("aa".repeat(100))
        .build();

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Script sig too large")
        );
    }

    #[test]
    fn split_reassembles_with_zero_extranonce2() {
        let (tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            500_000,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .with_pool_sig("|parasite|".into())
        .build()
        .unwrap();

        let extranonce1 = hex::decode("abcd1234").unwrap();
        let extranonce2 = vec![0u8; 8];

        let full = {
            let mut v = hex::decode(&coinb1).unwrap();
            v.extend_from_slice(&extranonce1);
            v.extend_from_slice(&extranonce2);
            v.extend_from_slice(&hex::decode(&coinb2).unwrap());
            v
        };

        pretty_assert_eq!(full, bitcoin::consensus::serialize(&tx));
    }

    #[test]
    fn split_allows_custom_extranonce2() {
        let (tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            0,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .build()
        .unwrap();

        let extranonce1 = hex::decode("abcd1234").unwrap();
        let extranonce2_custom = [0x11u8; 8];

        let joined = {
            let mut v = hex::decode(&coinb1).unwrap();
            v.extend_from_slice(&extranonce1);
            v.extend_from_slice(&extranonce2_custom);
            v.extend_from_slice(&hex::decode(&coinb2).unwrap());
            v
        };

        let original = bitcoin::consensus::serialize(&tx);
        assert_eq!(joined.len(), original.len(), "length must match");
        assert_ne!(
            joined, original,
            "bytes should differ when extranonce2 != zeros"
        );
    }

    #[test]
    fn deterministic_with_same_inputs() {
        let base = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            0,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        );

        let (tx1, c1_1, c1_2) = base.clone().build().unwrap();
        let (tx2, c2_1, c2_2) = base.build().unwrap();

        assert_eq!(
            bitcoin::consensus::serialize(&tx1),
            bitcoin::consensus::serialize(&tx2)
        );
        assert_eq!(c1_1, c2_1);
        assert_eq!(c1_2, c2_2);
    }

    #[test]
    fn aux_invalid_hex_errors() {
        let mut aux = BTreeMap::new();
        aux.insert("bad".to_string(), "zz".to_string());

        let err = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            800_000,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .with_aux(aux)
        .build()
        .unwrap_err()
        .to_string();

        assert!(err.contains("Invalid character"));
    }

    #[test]
    fn coinb1_ends_before_extranonce1() {
        let (_tx, coinb1, _coinb2) = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            1_000_000,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .build()
        .unwrap();

        assert!(
            !coinb1.contains("abcd1234"),
            "coinb1 must end before extranonce1 bytes"
        );
    }

    #[test]
    fn extranonce2_boundary_occurs_once() {
        let (tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            900_000,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .build()
        .unwrap();

        let extranonce1 = hex::decode("abcd1234").unwrap();
        let extranonce2_zeros = vec![0u8; 8];

        let mut full = hex::decode(&coinb1).unwrap();
        full.extend_from_slice(&extranonce1);
        full.extend_from_slice(&extranonce2_zeros);
        full.extend_from_slice(&hex::decode(&coinb2).unwrap());

        let bin = bitcoin::consensus::serialize(&tx);
        pretty_assert_eq!(full, bin);

        let mut needle = extranonce1.clone();
        needle.extend_from_slice(&extranonce2_zeros);

        let count = bin
            .windows(needle.len())
            .filter(|w| *w == needle.as_slice())
            .count();

        assert_eq!(count, 1, "extranonce1 || zeros should appear exactly once");
    }

    #[test]
    fn pool_sig_is_present_when_set() {
        let tag = "|parasite|".as_bytes();
        let (tx, _, _) = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            0,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .with_pool_sig("|parasite|".into())
        .build()
        .unwrap();

        let ss = tx.input[0].script_sig.as_bytes();
        assert!(
            ss.windows(tag.len()).any(|w| w == tag),
            "pool signature bytes must be present in scriptSig"
        );
    }

    #[test]
    fn coinb1_prefix_bytes_constant_when_only_tail_changes() {
        let base = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            0,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        );

        let (_tx_a, c1_a, _c2_a) = base.clone().build().unwrap();
        let (_tx_b, c1_b, _c2_b) = base.clone().with_timestamp(42).build().unwrap();
        let (_tx_c, c1_c, _c2_c) = base.with_pool_sig("hello".into()).build().unwrap();

        let mut tmp = [0u8; 8];
        let hlen = write_scriptint(&mut tmp, 0);
        let aux_len = 0usize; // no aux in this test
        let script_prefix_len = 1 + hlen + aux_len;

        let b_a = hex::decode(&c1_a).unwrap();
        let b_b = hex::decode(&c1_b).unwrap();
        let b_c = hex::decode(&c1_c).unwrap();

        assert_eq!(
            &b_a[b_a.len() - script_prefix_len..],
            &b_b[b_b.len() - script_prefix_len..],
            "prefix bytes must be identical; only the VarInt(scriptSigLen) value before them may differ"
        );
        assert_eq!(
            &b_a[b_a.len() - script_prefix_len..],
            &b_c[b_c.len() - script_prefix_len..]
        );
    }

    #[test]
    fn coinb1_varint_len_stable_but_value_changes_with_tail() {
        let base = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            0,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        );

        let (tx_a, c1_a, _) = base.clone().build().unwrap();
        let (tx_b, c1_b, _) = base.clone().with_timestamp(42).build().unwrap();

        let ss_a = tx_a.input[0].script_sig.len() as u64;
        let ss_b = tx_b.input[0].script_sig.len() as u64;

        assert_eq!(VarInt(ss_a).size(), VarInt(ss_b).size());
        assert_ne!(c1_a, c1_b);
    }

    #[test]
    fn join_roundtrip_various_extranonce2_sizes() {
        for x2 in [0usize, 1, 8, 16, 32] {
            let (tx, c1, c2) = CoinbaseBuilder::new(
                address(),
                "abcd1234".parse().unwrap(),
                x2,
                0,
                Amount::from_sat(50 * COIN_VALUE),
                ScriptBuf::new(),
            )
            .build()
            .unwrap();

            let x1 = hex::decode("abcd1234").unwrap();
            let x2_zeros = vec![0u8; x2];

            let mut full = hex::decode(&c1).unwrap();
            full.extend_from_slice(&x1);
            full.extend_from_slice(&x2_zeros);
            full.extend_from_slice(&hex::decode(&c2).unwrap());

            pretty_assert_eq!(full, bitcoin::consensus::serialize(&tx));
        }
    }

    #[test]
    fn aux_bytes_extend_prefix_and_shift_boundary() {
        let base = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            0,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        );

        let (_, c1_base, _) = base.clone().build().unwrap();

        let mut aux = BTreeMap::new();
        aux.insert("a".into(), "00112233".into());
        let (_, c1_aux, _) = base.with_aux(aux).build().unwrap();

        assert_eq!(c1_aux.len(), c1_base.len() + 2 * 4);
    }

    #[test]
    fn offset_matches_varint_formula() {
        let height = 600_000u64;

        let mut aux = BTreeMap::new();
        aux.insert("k".into(), "cafebabe".into());

        let (tx, c1, _c2) = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            height,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .with_aux(aux.clone())
        .with_pool_sig("hey".into())
        .with_timestamp(1)
        .build()
        .unwrap();

        let script_sig_len = tx.input[0].script_sig.len();

        let mut tmp = [0u8; 8];
        let hlen = write_scriptint(&mut tmp, height.try_into().unwrap());
        let aux_len: usize = aux.values().map(|h| hex::decode(h).unwrap().len()).sum();
        let script_prefix_len = 1 + hlen + aux_len;

        let expected_offset =
            4 + VarInt(1).size() + 36 + VarInt(script_sig_len as u64).size() + script_prefix_len;

        assert_eq!(
            c1.len() / 2,
            expected_offset,
            "coinb1 byte length must equal computed offset"
        );
    }

    #[test]
    fn pool_sig_resides_after_boundary() {
        let tag = "|parasite|";
        let (_tx, c1, c2) = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            0,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .with_pool_sig(tag.into())
        .build()
        .unwrap();

        let tag_hex = hex::encode(tag.as_bytes());
        assert!(!c1.contains(&tag_hex), "pool sig must not be in coinb1");
        assert!(c2.contains(&tag_hex), "pool sig must be in coinb2");
    }

    #[test]
    fn coinbase_script_sig_too_large_via_extranonce2_errors() {
        let extranonce2_size = CoinbaseBuilder::MAX_COINBASE_SCRIPT_SIG_SIZE;
        let err = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            extranonce2_size,
            2222,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .build()
        .unwrap_err()
        .to_string();

        assert!(err.contains("Script sig too large"));
    }

    #[test]
    fn coinbase_script_sig_too_large_via_aux_bytes_errors() {
        let err = CoinbaseBuilder::new(
            address(),
            "abcd1234".parse().unwrap(),
            8,
            1111,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .with_aux(
            [(
                "pad".to_string(),
                "00".repeat(CoinbaseBuilder::MAX_COINBASE_SCRIPT_SIG_SIZE),
            )]
            .into_iter()
            .collect(),
        )
        .build()
        .unwrap_err()
        .to_string();

        assert!(err.contains("Script sig too large"));
    }
}
