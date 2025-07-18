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
        let span =
            tracing::info_span!("hasher", job_id = %self.job_id, extranonce2 = %self.extranonce2);
        let _enter = span.enter();

        let mut hashes = 0u64;
        let start = Instant::now();
        let mut next_log = start + Duration::from_secs(10);

        loop {
            if cancel.is_cancelled() {
                return Err(anyhow!("hasher cancelled"));
            }

            for _ in 0..10000 {
                let hash = self.header.block_hash();
                hashes += 1;

                if self.pool_target.is_met_by(hash) {
                    info!("Solved block with hash: {hash}");
                    return Ok((self.header, self.extranonce2.clone(), self.job_id.clone()));
                }

                self.header.nonce = self
                    .header
                    .nonce
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("nonce space exhausted"))?;
            }

            let now = Instant::now();
            if now >= next_log {
                let elapsed = now.duration_since(start).as_secs_f64().max(1e-6);
                info!("Hashrate: {}", HashRate(hashes as f64 / elapsed));
                next_log += Duration::from_secs(10);
            }
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

    fn target_with_difficulty(difficulty: u8) -> Target {
        assert!(difficulty <= 32, "difficulty too high");

        let mut bytes = [0xFFu8; 32];

        let full_zero_bytes = (difficulty / 8) as usize;
        let partial_bits = difficulty % 8;

        for byte in bytes.iter_mut().take(full_zero_bytes) {
            *byte = 0x00;
        }

        if partial_bits > 0 {
            let mask = 0xFF >> partial_bits;
            bytes[full_zero_bytes] = mask;
        }

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
    fn test_target_difficulty_levels() {
        let target_0 = target_with_difficulty(0);
        let target_8 = target_with_difficulty(8);
        let target_16 = target_with_difficulty(16);
        let target_24 = target_with_difficulty(24);

        assert!(target_8 < target_0);
        assert!(target_16 < target_8);
        assert!(target_24 < target_16);

        let bytes_8 = target_8.to_be_bytes();
        let bytes_16 = target_16.to_be_bytes();

        assert_eq!(bytes_8[0], 0);
        assert_eq!(bytes_16[0], 0);
        assert_eq!(bytes_16[1], 0);

        assert_eq!(bytes_8[1], 0xFF);
        assert_eq!(bytes_16[2], 0xFF);
    }

    #[test]
    fn test_partial_byte_difficulty() {
        let target_4 = target_with_difficulty(4);
        let target_12 = target_with_difficulty(12);

        let bytes_4 = target_4.to_be_bytes();
        let bytes_12 = target_12.to_be_bytes();

        assert_eq!(bytes_4[0], 0x0F);
        assert_eq!(bytes_4[1], 0xFF);

        assert_eq!(bytes_12[0], 0);
        assert_eq!(bytes_12[1], 0x0F);
        assert_eq!(bytes_12[2], 0xFF);
    }

    #[test]
    fn hasher_hashes_with_very_low_difficulty() {
        let target = target_with_difficulty(1);
        let mut hasher = Hasher {
            header: header(None, None),
            pool_target: target,
            extranonce2: "00000000000".into(),
            job_id: "bf".into(),
        };

        let (header, _extranonce2, _job_id) = hasher.hash(CancellationToken::new()).unwrap();
        assert!(target.is_met_by(header.block_hash()));
    }

    #[test]
    fn hasher_nonce_space_exhausted() {
        let target = target_with_difficulty(32);
        let mut hasher = Hasher {
            header: header(None, Some(u32::MAX - 1)),
            pool_target: target,
            extranonce2: "00000000000".into(),
            job_id: "bg".into(),
        };

        assert!(
            hasher
                .hash(CancellationToken::new())
                .is_err_and(|err| err.to_string() == "nonce space exhausted")
        );
    }

    #[test]
    fn test_extreme_difficulties() {
        let easy_target = target_with_difficulty(1);
        let easy_bytes = easy_target.to_be_bytes();
        assert_eq!(easy_bytes[0], 0x7F);

        let hard_target = target_with_difficulty(32);
        let hard_bytes = hard_target.to_be_bytes();
        for byte in hard_bytes.iter().take(4) {
            assert_eq!(*byte, 0);
        }
        assert_eq!(hard_bytes[4], 0xFF);
    }

    #[test]
    fn test_hashrate_formatting() {
        assert_eq!(HashRate(1000.0).to_string(), "1.00 kH/s");
        assert_eq!(HashRate(1_000_000.0).to_string(), "1.00 MH/s");
        assert_eq!(HashRate(1_000_000_000.0).to_string(), "1.00 GH/s");
        assert_eq!(HashRate(1_000_000_000_000.0).to_string(), "1.00 TH/s");
        assert_eq!(HashRate(1_000_000_000_000_000.0).to_string(), "1.00 PH/s");
        assert_eq!(
            HashRate(1_000_000_000_000_000_000.0).to_string(),
            "1.00 EH/s"
        );
    }

    #[test]
    fn test_difficulty_progression() {
        let difficulties = [1, 4, 8, 12, 16, 20, 24];
        let mut targets = Vec::new();

        for &diff in &difficulties {
            targets.push(target_with_difficulty(diff));
        }

        for i in 1..targets.len() {
            assert!(
                targets[i] < targets[i - 1],
                "Target at difficulty {} should be smaller than difficulty {}",
                difficulties[i],
                difficulties[i - 1]
            );
        }
    }

    #[test]
    fn test_multiple_difficulty_levels() {
        let difficulties = [1, 2, 3, 4];

        for difficulty in difficulties {
            let target = target_with_difficulty(difficulty);
            let mut hasher = Hasher {
                header: header(None, None),
                pool_target: target,
                extranonce2: "00000000000".into(),
                job_id: format!("test_{difficulty}"),
            };

            let result = hasher.hash(CancellationToken::new());
            assert!(result.is_ok(), "Failed at difficulty {difficulty}");

            let (header, _, _) = result.unwrap();
            assert!(
                target.is_met_by(header.block_hash()),
                "Invalid PoW at difficulty {difficulty}"
            );
        }
    }
}
