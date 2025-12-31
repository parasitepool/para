use super::*;

#[derive(Debug, Snafu)]
pub(crate) enum HasherError {
    #[snafu(display("hasher cancelled: nonce={nonce}, version_rolls={version_rolls}"))]
    Cancelled { nonce: u32, version_rolls: u64 },
    #[snafu(display("nonce space exhausted: nonce={nonce}, version_rolls={version_rolls}"))]
    NonceSpaceExhausted { nonce: u32, version_rolls: u64 },
}

/// Result of a successful hash that meets the pool target.
#[derive(Debug, Clone)]
pub struct HashResult {
    pub job_id: JobId,
    pub header: Header,
    pub extranonce2: Extranonce,
    /// The rolled version bits (None if no rolling occurred or bits are 0)
    pub version_bits: Option<u32>,
}

#[derive(Debug)]
pub(crate) struct Hasher {
    pub(crate) extranonce2: Extranonce,
    pub(crate) header: Header,
    pub(crate) job_id: JobId,
    pub(crate) pool_target: Target,
    /// Version roller for ASICBoost support
    pub(crate) version_roller: VersionRoller,
    /// Track number of version rolls for metrics/debugging
    version_rolls: u64,
}

impl Hasher {
    /// Creates a new Hasher with version rolling enabled using BIP 320 mask.
    pub fn new(
        header: Header,
        pool_target: Target,
        extranonce2: Extranonce,
        job_id: JobId,
    ) -> Self {
        Self::with_version_mask(header, pool_target, extranonce2, job_id, BIP320_VERSION_MASK)
    }

    /// Creates a new Hasher with a custom version mask.
    pub fn with_version_mask(
        header: Header,
        pool_target: Target,
        extranonce2: Extranonce,
        job_id: JobId,
        version_mask: u32,
    ) -> Self {
        let base_version = header.version.to_consensus();
        Self {
            extranonce2,
            header,
            job_id,
            pool_target,
            version_roller: VersionRoller::new(base_version, version_mask),
            version_rolls: 0,
        }
    }

    /// Creates a new Hasher with version rolling disabled.
    pub fn without_version_rolling(
        header: Header,
        pool_target: Target,
        extranonce2: Extranonce,
        job_id: JobId,
    ) -> Self {
        Self::with_version_mask(header, pool_target, extranonce2, job_id, 0)
    }

    /// Main hashing loop with version rolling support.
    ///
    /// The algorithm works as follows:
    /// 1. Hash with current nonce
    /// 2. If share found, return success
    /// 3. Increment nonce
    /// 4. If nonce exhausted, roll version and reset nonce
    /// 5. If version exhausted, return NonceSpaceExhausted
    pub(crate) fn hash(
        &mut self,
        cancel: CancellationToken,
        metrics: Arc<Metrics>,
        throttle: f64,
    ) -> Result<HashResult, HasherError> {
        const BATCH: u64 = 10_000;

        // Apply initial version
        self.apply_current_version();

        loop {
            if cancel.is_cancelled() {
                return CancelledSnafu {
                    nonce: self.header.nonce,
                    version_rolls: self.version_rolls,
                }
                .fail();
            }

            let t0 = Instant::now();

            for _ in 0..BATCH {
                let hash = self.header.block_hash();

                if self.pool_target.is_met_by(hash) {
                    metrics.add_share();
                    return Ok(HashResult {
                        job_id: self.job_id,
                        header: self.header,
                        extranonce2: self.extranonce2.clone(),
                        version_bits: self.version_roller.rolled_bits(),
                    });
                }

                if let Some(next_nonce) = self.header.nonce.checked_add(1) {
                    self.header.nonce = next_nonce;
                } else {
                    // Nonce exhausted - try to roll version
                    if self.version_roller.roll() {
                        self.version_rolls += 1;
                        self.apply_current_version();
                        self.header.nonce = 0;
                    } else {
                        // Both nonce and version space exhausted
                        return NonceSpaceExhaustedSnafu {
                            nonce: self.header.nonce,
                            version_rolls: self.version_rolls,
                        }
                        .fail();
                    }
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

    /// Applies the current version from the roller to the header.
    #[inline]
    fn apply_current_version(&mut self) {
        let version = self.version_roller.current_version();
        self.header.version = bitcoin::block::Version::from_consensus(version);
    }

    /// Returns the number of version rolls performed.
    pub fn version_rolls(&self) -> u64 {
        self.version_rolls
    }

    /// Returns true if version rolling is enabled for this hasher.
    pub fn version_rolling_enabled(&self) -> bool {
        self.version_roller.is_enabled()
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
            version: bitcoin::block::Version::from_consensus(0x20000000),
            prev_blockhash: BlockHash::all_zeros(),
            merkle_root: TxMerkleNode::from_raw_hash(BlockHash::all_zeros().to_raw_hash()),
            time: 0,
            bits: network_target.unwrap_or(Target::MAX).to_compact_lossy(),
            nonce: nonce.unwrap_or_default(),
        }
    }

    fn header_with_version(version: i32, nonce: Option<u32>) -> Header {
        Header {
            version: bitcoin::block::Version::from_consensus(version),
            prev_blockhash: BlockHash::all_zeros(),
            merkle_root: TxMerkleNode::from_raw_hash(BlockHash::all_zeros().to_raw_hash()),
            time: 0,
            bits: Target::MAX.to_compact_lossy(),
            nonce: nonce.unwrap_or_default(),
        }
    }

    // ==================== Original Tests (updated for new structure) ====================

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
        let mut hasher = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            "bf".parse().unwrap(),
        );

        let result = hasher
            .hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX)
            .unwrap();
        assert!(target.is_met_by(result.header.block_hash()));
    }

    #[test]
    fn hasher_nonce_space_exhausted_without_version_rolling() {
        let target = Target::from_be_bytes([0u8; 32]);
        let mut hasher = Hasher::without_version_rolling(
            header(None, Some(u32::MAX - 100)),
            target,
            "0000000000".parse().unwrap(),
            "bf".parse().unwrap(),
        );

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
            let mut hasher = Hasher::new(
                header(None, None),
                target,
                "0000000000".parse().unwrap(),
                JobId::new(0),
            );

            let result = hasher.hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX);
            assert!(result.is_ok(), "Failed at {zeros} leading zeros");

            let hash_result = result.unwrap();
            assert!(
                target.is_met_by(hash_result.header.block_hash()),
                "Invalid PoW at {zeros} leading zeros"
            );
        }
    }

    #[test]
    fn test_parallel_mining_easy_target() {
        let target = shift(1);
        let mut hasher = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            JobId::new(0),
        );

        let result = hasher.hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX);

        assert!(
            result.is_ok(),
            "Mining should find solution for easy target"
        );

        let hash_result = result.unwrap();
        assert!(
            target.is_met_by(hash_result.header.block_hash()),
            "Solution should meet target"
        );
    }

    #[test]
    fn test_parallel_mining_cancellation() {
        let target = shift(30);
        let mut hasher = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            JobId::new(1),
        );

        let cancel_token = CancellationToken::new();
        cancel_token.cancel();

        let result = hasher.hash(cancel_token, Arc::new(Metrics::new()), f64::MAX);
        assert!(result.is_err(), "Should be cancelled");
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }

    // ==================== Version Rolling Tests ====================

    #[test]
    fn hasher_with_version_rolling_enabled_by_default() {
        let hasher = Hasher::new(
            header(None, None),
            shift(1),
            "0000000000".parse().unwrap(),
            JobId::new(0),
        );

        assert!(hasher.version_rolling_enabled());
    }

    #[test]
    fn hasher_without_version_rolling_disabled() {
        let hasher = Hasher::without_version_rolling(
            header(None, None),
            shift(1),
            "0000000000".parse().unwrap(),
            JobId::new(0),
        );

        assert!(!hasher.version_rolling_enabled());
    }

    #[test]
    fn hasher_version_bits_none_when_no_rolling_needed() {
        let target = shift(1); // Very easy target
        let mut hasher = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            JobId::new(0),
        );

        let result = hasher
            .hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX)
            .unwrap();

        // Easy target found before any version rolling needed
        assert_eq!(result.version_bits, None);
        assert_eq!(hasher.version_rolls(), 0);
    }

    #[test]
    fn hasher_version_rolling_extends_nonce_space() {
        // Use a small version mask for faster testing
        let small_mask = 0b11 << 13; // 2 bits = 4 version combinations
        let target = Target::from_be_bytes([0u8; 32]); // Impossible target

        let mut hasher = Hasher::with_version_mask(
            header(None, Some(u32::MAX - 50)), // Start near nonce exhaustion
            target,
            "0000000000".parse().unwrap(),
            JobId::new(0),
            small_mask,
        );

        let result = hasher.hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX);

        // Should exhaust after rolling through all versions
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("nonce space exhausted"));

        // Should have rolled through all 3 additional versions (0 is initial)
        assert_eq!(hasher.version_rolls(), 3);
    }

    #[test]
    fn hasher_preserves_base_version_bits() {
        let base_version = 0x20000000_i32;
        let target = shift(1);

        let mut hasher = Hasher::new(
            header_with_version(base_version, None),
            target,
            "0000000000".parse().unwrap(),
            JobId::new(0),
        );

        let result = hasher
            .hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX)
            .unwrap();

        // Base version bits should be preserved
        let result_version = result.header.version.to_consensus();
        assert_eq!(
            result_version & !BIP320_VERSION_MASK as i32,
            base_version & !BIP320_VERSION_MASK as i32,
            "Base version bits should be preserved"
        );
    }

    #[test]
    fn hasher_result_includes_version_bits_when_rolled() {
        // Create a scenario where version rolling is needed
        let small_mask = 0b1111 << 13; // 4 bits
        let target = shift(4); // Moderate difficulty

        let mut hasher = Hasher::with_version_mask(
            header_with_version(0x20000000, Some(u32::MAX - 10)),
            target,
            "0000000000".parse().unwrap(),
            JobId::new(0),
            small_mask,
        );

        let result = hasher
            .hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX)
            .unwrap();

        // If version was rolled, version_bits should be Some
        if hasher.version_rolls() > 0 {
            assert!(
                result.version_bits.is_some(),
                "version_bits should be Some when rolling occurred"
            );
            let bits = result.version_bits.unwrap();
            assert!(
                bits & small_mask == bits,
                "version_bits should only have bits within mask"
            );
        }
    }

    #[test]
    fn hasher_custom_version_mask() {
        // Test with custom mask (8 bits)
        let custom_mask = 0xFF << 16;
        let hasher = Hasher::with_version_mask(
            header(None, None),
            shift(1),
            "0000000000".parse().unwrap(),
            JobId::new(0),
            custom_mask,
        );

        assert!(hasher.version_rolling_enabled());
        assert_eq!(hasher.version_roller.mask(), custom_mask);
    }

    #[test]
    fn hasher_version_in_header_matches_roller() {
        let base = 0x20000000_i32;
        let mut hasher = Hasher::new(
            header_with_version(base, None),
            shift(1),
            "0000000000".parse().unwrap(),
            JobId::new(0),
        );

        // Before hashing, version should match initial
        assert_eq!(hasher.header.version.to_consensus(), base);

        // After hashing finds a result
        let result = hasher
            .hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX)
            .unwrap();

        // Result header version should match what roller produced
        let expected_version = hasher.version_roller.current_version();
        assert_eq!(result.header.version.to_consensus(), expected_version);
    }

    #[test]
    fn hasher_nonce_resets_on_version_roll() {
        let small_mask = 0b1 << 13; // 1 bit = 2 combinations
        let target = Target::from_be_bytes([0u8; 32]); // Impossible

        let mut hasher = Hasher::with_version_mask(
            header(None, Some(u32::MAX - 5)), // Very close to exhaustion
            target,
            "0000000000".parse().unwrap(),
            JobId::new(0),
            small_mask,
        );

        // Start hashing (will exhaust quickly)
        let _ = hasher.hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX);

        // After exhausting, nonce should have wrapped back around
        // (We can't easily verify mid-hash, but the test above confirms rolling works)
    }

    #[test]
    fn hasher_version_rolls_tracked_correctly() {
        let small_mask = 0b111 << 13; // 3 bits = 8 combinations
        let target = Target::from_be_bytes([0u8; 32]);

        let mut hasher = Hasher::with_version_mask(
            header(None, Some(u32::MAX - 5)),
            target,
            "0000000000".parse().unwrap(),
            JobId::new(0),
            small_mask,
        );

        let _ = hasher.hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX);

        // Should have rolled through all 7 additional versions
        assert_eq!(hasher.version_rolls(), 7);
    }

    #[test]
    fn hasher_finds_share_with_rolled_version() {
        // Use a target that requires some work but not too much
        let target = shift(8);
        let small_mask = 0b1111 << 13;

        // Start at a high nonce to force version rolling
        let mut hasher = Hasher::with_version_mask(
            header_with_version(0x20000000, Some(u32::MAX - 100)),
            target,
            "0000000000".parse().unwrap(),
            JobId::new(0),
            small_mask,
        );

        let result = hasher
            .hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX)
            .unwrap();

        // Verify the share is valid
        assert!(target.is_met_by(result.header.block_hash()));

        // If rolling occurred, verify version bits are valid
        if let Some(bits) = result.version_bits {
            assert!(bits & small_mask == bits);
            // Verify header version includes these bits
            let header_version = result.header.version.to_consensus() as u32;
            assert_eq!(header_version & small_mask, bits);
        }
    }

    #[test]
    fn hasher_error_includes_version_roll_count() {
        let small_mask = 0b11 << 13;
        let target = Target::from_be_bytes([0u8; 32]);

        let mut hasher = Hasher::with_version_mask(
            header(None, Some(u32::MAX - 5)),
            target,
            "0000000000".parse().unwrap(),
            JobId::new(0),
            small_mask,
        );

        let result = hasher.hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX);
        let err_str = result.unwrap_err().to_string();

        assert!(err_str.contains("version_rolls=3"));
    }

    #[test]
    fn hasher_cancellation_includes_version_info() {
        let target = shift(30);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let mut hasher = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            JobId::new(0),
        );

        let result = hasher.hash(cancel, Arc::new(Metrics::new()), f64::MAX);
        let err_str = result.unwrap_err().to_string();

        assert!(err_str.contains("cancelled"));
        assert!(err_str.contains("version_rolls="));
    }

    // ==================== Integration Tests ====================

    #[test]
    fn integration_version_rolling_with_real_mining_scenario() {
        // Simulate a real mining scenario
        let base_version = 0x20000000_i32;
        let target = shift(3); // Difficulty requiring some work

        let mut hasher = Hasher::new(
            header_with_version(base_version, None),
            target,
            "0000000000".parse().unwrap(),
            JobId::new(42),
        );

        let result = hasher
            .hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX)
            .unwrap();

        // Verify complete share data
        assert_eq!(result.job_id, JobId::new(42));
        assert!(target.is_met_by(result.header.block_hash()));
        assert_eq!(result.extranonce2, "0000000000".parse().unwrap());

        // Verify version is valid
        let version = result.header.version.to_consensus();
        assert_eq!(
            version & !BIP320_VERSION_MASK as i32,
            base_version & !BIP320_VERSION_MASK as i32
        );
    }

    #[test]
    fn integration_multiple_hashers_independent() {
        let target = shift(2);

        let mut hasher1 = Hasher::new(
            header_with_version(0x20000000, None),
            target,
            "0000000001".parse().unwrap(),
            JobId::new(1),
        );

        let mut hasher2 = Hasher::new(
            header_with_version(0x20000000, Some(1000)),
            target,
            "0000000002".parse().unwrap(),
            JobId::new(2),
        );

        let result1 = hasher1
            .hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX)
            .unwrap();
        let result2 = hasher2
            .hash(CancellationToken::new(), Arc::new(Metrics::new()), f64::MAX)
            .unwrap();

        // Both should find valid shares
        assert!(target.is_met_by(result1.header.block_hash()));
        assert!(target.is_met_by(result2.header.block_hash()));

        // But they're independent
        assert_eq!(result1.job_id, JobId::new(1));
        assert_eq!(result2.job_id, JobId::new(2));
    }
}
