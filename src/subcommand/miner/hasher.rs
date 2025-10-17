use super::*;

#[derive(Debug)]
pub(crate) struct Hasher {
    pub(crate) extranonce2: Extranonce,
    pub(crate) header: Header,
    pub(crate) job_id: JobId,
    pub(crate) pool_target: Target,
}

impl Hasher {
    pub(crate) fn hash(
        &mut self,
        cancel: CancellationToken,
    ) -> Result<(JobId, Header, Extranonce)> {
        let mut hashes = 0;
        let start = Instant::now();
        let mut last_log = start;

        let span =
            tracing::info_span!("hasher", job_id = %self.job_id, extranonce2 = %self.extranonce2);
        let _ = span.enter();

        loop {
            if cancel.is_cancelled() {
                return Err(anyhow!("hasher cancelled"));
            }

            for _ in 0..10000 {
                let hash = self.header.block_hash();
                hashes += 1;

                if self.pool_target.is_met_by(hash) {
                    info!("Solved block with hash: {hash}");
                    return Ok((self.job_id, self.header, self.extranonce2.clone()));
                }

                self.header.nonce = self
                    .header
                    .nonce
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("nonce space exhausted"))?;
            }

            let now = Instant::now();
            let elapsed_since_last = now.duration_since(last_log).as_secs();

            if elapsed_since_last >= 5 {
                let total_elapsed = now.duration_since(start).as_secs_f64().max(1e-6);
                let current_hashrate = hashes as f64 / total_elapsed;

                info!("Hashrate: {}", HashRate(current_hashrate));

                last_log = now;
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

    fn shift(leading_zeros: u8) -> Target {
        assert!(leading_zeros <= 32, "leading_zeros too high");

        let mut bytes = [0xFFu8; 32];

        let full_zero_bytes = (leading_zeros / 8) as usize;
        let partial_bits = leading_zeros % 8;

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
    fn test_target_leading_zeros_levels() {
        let target_0 = shift(0);
        let target_8 = shift(8);
        let target_16 = shift(16);
        let target_24 = shift(24);

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
    fn test_partial_byte_leading_zeros() {
        let target_4 = shift(4);
        let target_12 = shift(12);

        let bytes_4 = target_4.to_be_bytes();
        let bytes_12 = target_12.to_be_bytes();

        assert_eq!(bytes_4[0], 0x0F);
        assert_eq!(bytes_4[1], 0xFF);

        assert_eq!(bytes_12[0], 0);
        assert_eq!(bytes_12[1], 0x0F);
        assert_eq!(bytes_12[2], 0xFF);
    }

    #[test]
    fn hasher_hashes_with_very_low_leading_zeros() {
        let target = shift(1);
        let mut hasher = Hasher {
            header: header(None, None),
            pool_target: target,
            extranonce2: "0000000000".parse().unwrap(),
            job_id: "bf".parse().unwrap(),
        };

        let (_job_id, header, _extranonce2) = hasher.hash(CancellationToken::new()).unwrap();
        assert!(target.is_met_by(header.block_hash()));
    }

    #[test]
    fn hasher_nonce_space_exhausted() {
        let target = shift(32);
        let mut hasher = Hasher {
            header: header(None, Some(u32::MAX - 1)),
            pool_target: target,
            extranonce2: "0000000000".parse().unwrap(),
            job_id: "bb".parse().unwrap(),
        };

        assert!(
            hasher
                .hash(CancellationToken::new())
                .is_err_and(|err| err.to_string() == "nonce space exhausted")
        );
    }

    #[test]
    fn test_extreme_leading_zeros() {
        let easy_target = shift(1);
        let easy_bytes = easy_target.to_be_bytes();
        assert_eq!(easy_bytes[0], 0x7F);

        let hard_target = shift(32);
        let hard_bytes = hard_target.to_be_bytes();
        for byte in hard_bytes.iter().take(4) {
            assert_eq!(*byte, 0);
        }
        assert_eq!(hard_bytes[4], 0xFF);
    }

    #[test]
    fn test_leading_zeros_progression() {
        let leading_zeros = [1, 4, 8, 12, 16, 20, 24];
        let mut targets = Vec::new();

        for &zeros in &leading_zeros {
            targets.push(shift(zeros));
        }

        for i in 1..targets.len() {
            assert!(
                targets[i] < targets[i - 1],
                "Target at {} leading zeros should be smaller than {} leading zeros",
                leading_zeros[i],
                leading_zeros[i - 1]
            );
        }
    }

    #[test]
    fn test_multiple_leading_zeros_levels() {
        let leading_zeros = [1, 2, 3, 4];

        for zeros in leading_zeros {
            let target = shift(zeros);
            let mut hasher = Hasher {
                header: header(None, None),
                pool_target: target,
                extranonce2: "0000000000".parse().unwrap(),
                job_id: "abc".parse().unwrap(),
            };

            let result = hasher.hash(CancellationToken::new());
            assert!(result.is_ok(), "Failed at {zeros} leading zeros");

            let (_, header, _) = result.unwrap();
            assert!(
                target.is_met_by(header.block_hash()),
                "Invalid PoW at {zeros} leading zeros"
            );
        }
    }
}
