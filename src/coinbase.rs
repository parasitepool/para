use super::*;

pub struct CoinbaseBuilder {
    pub address: Address,
    pub aux: HashMap<String, String>,
    pub extranonce1: String,
    pub extranonce2_size: usize,
    pub height: u64,
    pub value: Amount,
    pub witness_commitment: ScriptBuf,
}

impl CoinbaseBuilder {
    pub fn build(self) -> Result<(Transaction, String, String)> {
        let mut offset = 4 + 36; // tx version len + previous_output
        // TODO: just use a Vec and convert to ScriptBuf at the end
        // BIP34 encode block height
        let mut buf = [0u8; 8];
        let len = write_scriptint(&mut buf, self.height.try_into().unwrap());
        let mut builder = Builder::new().push_slice(&<&PushBytes>::from(&buf)[..len]);

        for (_, value) in self.aux.into_iter() {
            let mut buf = PushBytesBuf::new();
            buf.extend_from_slice(hex::decode(value)?.as_slice())?;
            builder = builder.push_slice(buf);
        }

        offset += builder.len();

        let mut buf = PushBytesBuf::new();
        buf.extend_from_slice(hex::decode(self.extranonce1)?.as_slice())?;
        builder = builder.push_slice(buf);

        let mut buf = PushBytesBuf::new();
        buf.extend_from_slice(vec![0u8; self.extranonce2_size].as_slice())?;
        builder = builder.push_slice(buf);

        // not necessarily in that order
        // TODO: timestamp
        // TODO: unique randomiser based on the nsec timestamp
        // TODO: hidden |parasite| sig (use hex values)
        // TODO: configurabe pool sig

        let script_sig = builder.into_script();

        assert!(script_sig.len() <= MAX_COINBASE_INPUT_SIZE);

        let input = TxIn {
            previous_output: OutPoint::null(),
            script_sig,
            sequence: Sequence::MAX, // TODO: MAX or ZERO?
            witness: Witness::new(), // TODO?
        };

        let reward = TxOut {
            value: self.value,
            script_pubkey: self.address.script_pubkey(),
        };

        let wtxid_op_return = TxOut {
            value: Amount::ZERO,
            script_pubkey: self.witness_commitment,
        };

        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![input],
            output: vec![reward, wtxid_op_return],
        };

        let bin = consensus::serialize(&tx);

        let coinb1 = hex::encode(&bin[..offset]);
        let coinb2 = hex::encode(&bin[offset + EXTRANONCE2_SIZE + 2..]);

        Ok((tx, coinb1, coinb2))
    }
}
