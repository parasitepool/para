use {super::*, rand::Rng};

#[derive(Debug, Snafu)]
pub(crate) enum HasherError {
    #[snafu(display("hasher cancelled: nonce={nonce}"))]
    Cancelled { nonce: u32 },
    #[snafu(display("nonce space exhausted: nonce={nonce}"))]
    NonceSpaceExhausted { nonce: u32 },
}

#[derive(Debug)]
pub(crate) struct Hasher {
    pub(crate) enonce2: Extranonce,
    pub(crate) header: Header,
    pub(crate) job_id: JobId,
    pub(crate) pool_target: Target,
    pub(crate) version: Version,
    pub(crate) version_mask: Option<Version>,
}

impl Hasher {
    pub(crate) fn hash(
        &mut self,
        cancel: CancellationToken,
        metrics: Arc<Metrics>,
        throttle: f64,
    ) -> Result<(JobId, Header, Extranonce, Option<Version>), HasherError> {
        const BATCH: u64 = 10_000;

        let mut rng = rand::rng();

        let mut current_version_bits = None;

        loop {
            if cancel.is_cancelled() {
                return CancelledSnafu {
                    nonce: self.header.nonce,
                }
                .fail();
            }

            if let Some(mask) = self.version_mask {
                let random_bits = Version::from(rng.random::<i32>());
                let version_bits = random_bits & mask;
                current_version_bits = Some(version_bits);

                self.header.version = ((self.version & !mask) | version_bits).into();
            }

            let t0 = Instant::now();

            for _ in 0..BATCH {
                let hash = self.header.block_hash();

                if self.pool_target.is_met_by(hash) {
                    metrics.add_share();
                    return Ok((
                        self.job_id,
                        self.header,
                        self.enonce2.clone(),
                        current_version_bits,
                    ));
                }

                if let Some(next_nonce) = self.header.nonce.checked_add(1) {
                    self.header.nonce = next_nonce;
                } else {
                    if self.version_mask.is_some() {
                        self.header.nonce = 0;
                        break;
                    }
                    return NonceSpaceExhaustedSnafu {
                        nonce: self.header.nonce,
                    }
                    .fail();
                }
            }

            metrics.add_hashes(BATCH);

            if throttle != f64::MAX {
                let want = (BATCH as f64) / throttle;
                let got = t0.elapsed().as_secs_f64();
                if want > got {
                    thread::sleep(Duration::from_secs_f64(want - got));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, bitcoin::TxMerkleNode};

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
            version: bitcoin::block::Version::TWO,
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
        let header = header(None, None);
        let mut hasher = Hasher {
            version: header.version.into(),
            header,
            pool_target: target,
            enonce2: "0000000000".parse().unwrap(),
            job_id: "bf".parse().unwrap(),
            version_mask: None,
        };

        let (_, header, _, _) = hasher
            .hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX)
            .unwrap();
        assert!(target.is_met_by(header.block_hash()));
    }

    #[test]
    fn hasher_nonce_space_exhausted() {
        let target = Target::from_be_bytes([0u8; 32]);
        let header = header(None, Some(u32::MAX - 100));
        let mut hasher = Hasher {
            version: header.version.into(),
            header,
            pool_target: target,
            enonce2: "0000000000".parse().unwrap(),
            job_id: "bf".parse().unwrap(),
            version_mask: None,
        };

        let result = hasher.hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX);

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
            let header = header(None, None);
            let mut hasher = Hasher {
                version: header.version.into(),
                header,
                pool_target: target,
                enonce2: "0000000000".parse().unwrap(),
                job_id: JobId::new(0),
                version_mask: None,
            };

            let result = hasher.hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX);
            assert!(result.is_ok(), "Failed at {zeros} leading zeros");

            let (_, header, _, _) = result.unwrap();
            assert!(
                target.is_met_by(header.block_hash()),
                "Invalid PoW at {zeros} leading zeros"
            );
        }
    }

    #[test]
    fn test_parallel_mining_easy_target() {
        let target = shift(1);
        let header = header(None, None);
        let mut hasher = Hasher {
            version: header.version.into(),
            header,
            pool_target: target,
            enonce2: "0000000000".parse().unwrap(),
            job_id: JobId::new(0),
            version_mask: None,
        };

        let result = hasher.hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX);

        assert!(
            result.is_ok(),
            "Mining should find solution for easy target"
        );

        let (_, header, _, _) = result.unwrap();
        assert!(
            target.is_met_by(header.block_hash()),
            "Solution should meet target"
        );
    }

    #[test]
    fn test_parallel_mining_cancellation() {
        let target = shift(30);
        let header = header(None, None);
        let mut hasher = Hasher {
            version: header.version.into(),
            header,
            pool_target: target,
            enonce2: "0000000000".parse().unwrap(),
            job_id: JobId::new(1),
            version_mask: None,
        };

        let cancel_token = CancellationToken::new();

        cancel_token.cancel();

        let result = hasher.hash(cancel_token, Arc::new(Metrics::new()), f64::MAX);
        assert!(result.is_err(), "Should be cancelled");
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }

    #[test]
    fn test_version_rolling_applies_mask() {
        let target = shift(1);
        let header = header(None, None);
        let mask = Version::from_str("1fffe000").unwrap();

        let mut hasher = Hasher {
            version: header.version.into(),
            header,
            pool_target: target,
            enonce2: "0000000000".parse().unwrap(),
            job_id: JobId::new(0),
            version_mask: Some(mask),
        };

        let result = hasher.hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX);
        assert!(result.is_ok(), "Mining with version rolling should succeed");

        let (_, header, _, version_bits) = result.unwrap();
        assert!(
            target.is_met_by(header.block_hash()),
            "Solution should meet target"
        );

        if let Some(version_bits) = version_bits {
            let disallowed = version_bits & !mask;
            assert_eq!(
                disallowed,
                Version::from(0),
                "version_bits should only contain bits within the mask"
            );
        }
    }
}
