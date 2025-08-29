use super::*;

// Should I test round tripping. Going from the bin tx back to this struct?

pub struct CoinbaseBuilder {
    address: Address,
    aux: HashMap<String, String>,
    extranonce1: String,
    extranonce2_size: usize,
    height: u64,
    pool_sig: Option<String>,
    randomiser: bool,
    timestamp: Option<u32>,
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
            aux: HashMap::new(),
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

    pub fn with_aux(mut self, aux: HashMap<String, String>) -> Self {
        self.aux = aux;
        self
    }

    #[allow(unused)]
    pub fn with_timestamp(mut self, timestamp: u32) -> Self {
        self.timestamp = Some(timestamp);
        self
    }

    #[allow(unused)]
    pub fn with_randomiser(mut self, randomiser: bool) -> Self {
        self.randomiser = randomiser;
        self
    }

    #[allow(unused)]
    pub fn with_pool_sig(mut self, pool_sig: String) -> Self {
        self.pool_sig = Some(pool_sig);
        self
    }

    pub fn build(self) -> Result<(Transaction, String, String)> {
        let mut offset = 4 + 36; // tx version len + previous_output

        let mut buf: Vec<u8> = Vec::new();

        let mut bip34_encoded_blockheight = [0u8; 8];
        let len = write_scriptint(
            &mut bip34_encoded_blockheight,
            self.height.try_into().unwrap(),
        );
        buf.extend_from_slice(&bip34_encoded_blockheight[..len]);

        for (_, value) in self.aux.into_iter() {
            buf.extend_from_slice(hex::decode(value)?.as_slice());
        }

        offset += buf.len();

        let extranonce1 = hex::decode(self.extranonce1)?;

        let total_extranonce_size = extranonce1.len() + self.extranonce2_size;

        buf.extend_from_slice(extranonce1.as_slice());
        buf.extend_from_slice(vec![0u8; self.extranonce2_size].as_slice());

        if let Some(_sig) = self.pool_sig {
            todo!();
        }

        // TODO: hidden |parasite| sig (use hex values)

        if let Some(ts) = self.timestamp {
            buf.extend_from_slice(&ts.to_be_bytes());
        }

        if self.randomiser {
            todo!();
        }

        let script_sig = ScriptBuf::from_bytes(buf);

        let script_sig_size = script_sig.len();

        info!("Script sig size: {script_sig_size}");

        assert!(script_sig_size <= Self::MAX_COINBASE_SCRIPT_SIG_SIZE);

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

        info!("Coinbase tx size: {}", bin.len());

        let coinb1 = hex::encode(&bin[..offset]);
        let coinb2 = hex::encode(&bin[offset + total_extranonce_size..]);

        Ok((coinbase, coinb1, coinb2))
    }
}
