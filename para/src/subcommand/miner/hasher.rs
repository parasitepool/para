use super::*;

#[derive(Debug)]
pub(crate) struct Hasher {
    pub(crate) extranonce2: String,
    pub(crate) header: Header,
    pub(crate) job_id: String,
    pub(crate) pool_target: Target,
}

impl Hasher {
    pub(crate) fn hash(&mut self, cancel: CancellationToken) -> Result<(Header, String, String)> {
        let mut hashes = 0;
        let start = Instant::now();
        let mut last_log = start;

        loop {
            if cancel.is_cancelled() {
                return Err(anyhow!("hasher cancelled"));
            }

            let hash = self.header.block_hash();
            hashes += 1;

            if self.pool_target.is_met_by(hash) {
                info!("Solved block with hash: {hash}");
                return Ok((self.header, self.extranonce2.clone(), self.job_id.clone()));
            }

            if self.header.nonce == u32::MAX {
                return Err(anyhow!("nonce space exhausted"));
            }

            let now = Instant::now();
            if now.duration_since(last_log) >= Duration::from_secs(10) {
                let elapsed = now.duration_since(start).as_secs_f64().max(1e-6);
                last_log = now;
                info!("Hashrate: {}", HashRate(hashes as f64 / elapsed));
            }
            self.header.nonce = self.header.nonce.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        bitcoin::{
            BlockHash, Target, TxMerkleNode,
            block::{Header, Version},
            hashes::Hash,
        },
    };

    // Hacky way to create very easy to solve targets for testing
    fn target(shift: u8) -> Target {
        let mut bytes = [0u8; 32];
        let (a, b, c, d) = match shift {
            0 => (0xff, 0xff, 0x00, 0x00),
            1 => (0x0f, 0xff, 0xf0, 0x00),
            2 => (0x00, 0xff, 0xff, 0x00),
            3 => (0x00, 0x0f, 0xff, 0xf0),
            4 => (0x00, 0x00, 0xff, 0xff),
            _ => panic!("shift should be less than 5"),
        };

        bytes[0] = a;
        bytes[1] = b;
        bytes[2] = c;
        bytes[3] = d;

        Target::from_be_bytes(bytes)
    }

    fn header(network_target: Option<Target>, nonce: Option<u32>) -> Header {
        Header {
            version: Version::TWO,
            prev_blockhash: BlockHash::all_zeros(),
            merkle_root: TxMerkleNode::from_raw_hash(BlockHash::all_zeros().to_raw_hash()),
            time: 0,
            bits: network_target.unwrap_or(Target::MAX).to_compact_lossy(),
            nonce: nonce.unwrap_or_default(),
        }
    }
    #[test]
    fn hasher_hashes() {
        let target = target(1);

        let mut hasher = Hasher {
            header: header(Some(target), None),
            pool_target: target,
            extranonce2: "00000000000".into(),
            job_id: "bf".into(),
        };

        let (header, _extranonce2, _job_id) = hasher.hash(CancellationToken::new()).unwrap();

        assert!(header.validate_pow(target).is_ok());
    }

    #[test]
    fn hasher_nonce_space_exhausted() {
        let target = target(1);

        let mut hasher = Hasher {
            header: header(Some(target), Some(u32::MAX - 1)),
            pool_target: target,
            extranonce2: "00000000000".into(),
            job_id: "bg".into(),
        };

        assert!(
            hasher
                .hash(CancellationToken::new())
                .is_err_and(|err| err.to_string() == "nonce space exhausted")
        )
    }
}
