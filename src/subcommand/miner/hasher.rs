use super::*;
use std::time::{Duration, Instant};

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
        let start = Instant::now();
        let mut total_hashes = 0u64;
        let mut last_report = start;
        const REPORT_INTERVAL: Duration = Duration::from_secs(5);

        loop {
            if cancel.is_cancelled() {
                return Err(anyhow!("hasher cancelled"));
            }

            let batch_size = if self.header.nonce > u32::MAX - 10000 {
                (u32::MAX - self.header.nonce) as usize + 1
            } else {
                10000
            };

            for _ in 0..batch_size {
                let hash = self.header.block_hash();
                total_hashes += 1;

                if self.pool_target.is_met_by(hash) {
                    info!("Solved block with hash: {hash}");
                    return Ok((self.job_id, self.header, self.extranonce2.clone()));
                }

                if let Some(next_nonce) = self.header.nonce.checked_add(1) {
                    self.header.nonce = next_nonce;
                } else {
                    return Err(anyhow!("nonce space exhausted"));
                }
            }

            if batch_size < 10000 {
                return Err(anyhow!("nonce space exhausted"));
            }

            let now = Instant::now();
            if now.duration_since(last_report) >= REPORT_INTERVAL {
                let elapsed = now.duration_since(start).as_secs_f64().max(1e-6);
                let hashrate = total_hashes as f64 / elapsed;
                info!("Hashrate: {}", HashRate(hashrate));
                last_report = now;
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
        let target = Target::from_be_bytes([0u8; 32]);
        let mut hasher = Hasher {
            header: header(None, Some(u32::MAX - 100)),
            pool_target: target,
            extranonce2: "0000000000".parse().unwrap(),
            job_id: "bg".parse().unwrap(),
        };

        let result = hasher.hash(CancellationToken::new());
        assert!(
            result.is_err(),
            "Expected nonce space exhausted error, got: {:?}",
            result
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("nonce space exhausted"),
            "Expected 'nonce space exhausted' error"
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
                job_id: JobId::new(0),
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

    #[test]
    fn test_parallel_mining_easy_target() {
        let target = shift(1);
        let mut hasher = Hasher {
            header: header(None, None),
            pool_target: target,
            extranonce2: "0000000000".parse().unwrap(),
            job_id: JobId::new(0),
        };

        let result = hasher.hash(CancellationToken::new());
        assert!(
            result.is_ok(),
            "Mining should find solution for easy target"
        );

        let (_, header, _) = result.unwrap();
        assert!(
            target.is_met_by(header.block_hash()),
            "Solution should meet target"
        );
    }

    #[test]
    fn test_parallel_mining_cancellation() {
        let target = shift(30);
        let mut hasher = Hasher {
            header: header(None, None),
            pool_target: target,
            extranonce2: "0000000000".parse().unwrap(),
            job_id: JobId::new(1),
        };

        let cancel_token = CancellationToken::new();

        cancel_token.cancel();

        let result = hasher.hash(cancel_token);
        assert!(result.is_err(), "Should be cancelled");
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }

    #[test]
    fn test_hashrate_display() {
        assert_eq!(format!("{}", HashRate(1500.0)), "1.500K");
        assert_eq!(format!("{}", HashRate(2_500_000.0)), "2.500M");
        assert_eq!(format!("{}", HashRate(3_200_000_000.0)), "3.200G");
        assert_eq!(format!("{}", HashRate(1_100_000_000_000.0)), "1.100T");
    }
}
