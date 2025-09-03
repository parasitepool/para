use super::*;

// Should I test round tripping. Going from the bin tx back to this struct?

// coinbase is made up of fixes size slots (variable sized slots?)

#[derive(Clone)]
pub struct CoinbaseBuilder {
    address: Address,
    aux: BTreeMap<String, String>,
    extranonce1: String,
    extranonce2_size: usize,
    height: u64,
    pool_sig: Option<String>,
    randomiser: bool,
    timestamp: Option<u64>,
    value: Amount,
    witness_commitment: ScriptBuf,
}

impl CoinbaseBuilder {
    const MAX_COINBASE_SCRIPT_SIG_SIZE: usize = 100;

    pub fn new(
        address: Address,
        extranonce1: String,
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
            randomiser: false,
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

    pub fn with_randomiser(mut self, randomiser: bool) -> Self {
        self.randomiser = randomiser;
        self
    }

    pub fn with_pool_sig(mut self, pool_sig: String) -> Self {
        self.pool_sig = Some(pool_sig);
        self
    }

    pub fn build(self) -> Result<(Transaction, String, String)> {
        // tx version len + vin count + previous output len
        let mut offset = 4 + 1 + 36 + 1; // TODO: the last + 1 is the size of varint of length of script

        let mut buf: Vec<u8> = Vec::with_capacity(Self::MAX_COINBASE_SCRIPT_SIG_SIZE);

        // BIP34
        let mut minimally_encoded_serialized_cscript = [0u8; 8];
        let len = write_scriptint(
            &mut minimally_encoded_serialized_cscript,
            self.height.try_into().expect("height should always fit"),
        );
        buf.push(len as u8); // byte length should be fine for the next 150 years
        buf.extend_from_slice(&minimally_encoded_serialized_cscript[..len]);

        for (_, value) in self.aux.into_iter() {
            buf.extend_from_slice(hex::decode(value)?.as_slice());
        }

        offset += buf.len();

        let extranonce1 = hex::decode(self.extranonce1)?;

        let total_extranonce_size = extranonce1.len() + self.extranonce2_size;

        buf.extend_from_slice(extranonce1.as_slice());
        buf.extend_from_slice(vec![0u8; self.extranonce2_size].as_slice());

        if let Some(sig) = self.pool_sig {
            buf.extend_from_slice(sig.as_bytes())
        }

        if let Some(ts) = self.timestamp {
            buf.extend_from_slice(&ts.to_le_bytes());
        }

        if self.randomiser {
            buf.extend_from_slice(
                &SystemTime::now()
                    .duration_since(UNIX_EPOCH)?
                    .as_nanos()
                    .to_le_bytes(),
            );
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
            "abcd1234".into(),
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
            "abcd1234".into(),
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
            "abcd1234".into(),
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
            "abcd1234".into(),
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
            "abcd1234".into(),
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
            "abcd1234".into(),
            8,
            1_000_000,
            Amount::from_sat(50 * COIN_VALUE),
            ScriptBuf::new(),
        )
        .build()
        .unwrap();

        let extranonce1_hex = "abcd1234";

        assert!(
            !coinb1.contains(extranonce1_hex),
            "coinb1 must end before extranonce1 bytes"
        );
    }

    #[test]
    fn extranonce2_boundary_occurs_once() {
        let (tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address(),
            "abcd1234".into(),
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
            "abcd1234".into(),
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
}
