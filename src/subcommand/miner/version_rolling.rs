/// BIP 320 default version rolling mask.
/// Bits 13-28 (16 bits) are designated for miner use.
/// This provides 2^16 = 65,536 additional nonce combinations per job.
pub const BIP320_VERSION_MASK: u32 = 0x1FFFE000;

/// Minimum number of bits required for effective version rolling.
/// Pools may specify this via mining.configure response.
pub const MIN_VERSION_BITS: u32 = 2;

/// Manages version rolling state for a single hashing unit.
///
/// Version rolling (overt ASICBoost) exploits the fact that certain bits
/// in the block version field can be freely modified by miners without
/// affecting block validity. This effectively multiplies the nonce space.
#[derive(Debug, Clone)]
pub struct VersionRoller {
    /// The base version from the mining job (with mask bits zeroed)
    base_version: i32,
    /// Current rolled value (only the bits within the mask)
    current_bits: u32,
    /// Maximum value for rolled bits (derived from mask)
    max_bits: u32,
    /// The version mask specifying which bits can be rolled
    mask: u32,
    /// Bit shift amount (position of lowest set bit in mask)
    shift: u32,
}

impl VersionRoller {
    /// Creates a new VersionRoller with the given base version and mask.
    ///
    /// # Arguments
    /// * `base_version` - The version from the mining job
    /// * `mask` - The version rolling mask (typically BIP320_VERSION_MASK)
    pub fn new(base_version: i32, mask: u32) -> Self {
        let shift = if mask == 0 { 0 } else { mask.trailing_zeros() };
        let max_bits = if mask == 0 { 0 } else { mask >> shift };

        Self {
            base_version: base_version & !(mask as i32),
            current_bits: 0,
            max_bits,
            mask,
            shift,
        }
    }

    /// Creates a VersionRoller with BIP 320 default mask.
    pub fn with_bip320_mask(base_version: i32) -> Self {
        Self::new(base_version, BIP320_VERSION_MASK)
    }

    /// Creates a disabled VersionRoller (no version rolling).
    pub fn disabled(base_version: i32) -> Self {
        Self::new(base_version, 0)
    }

    /// Returns the current complete version (base + rolled bits).
    #[inline]
    pub fn current_version(&self) -> i32 {
        self.base_version | ((self.current_bits << self.shift) as i32 & self.mask as i32)
    }

    /// Returns only the rolled bits portion (suitable for share submission).
    /// Returns None if version rolling is disabled or bits are zero.
    #[inline]
    pub fn rolled_bits(&self) -> Option<u32> {
        if self.mask == 0 || self.current_bits == 0 {
            None
        } else {
            Some(self.current_bits << self.shift)
        }
    }

    /// Attempts to roll to the next version.
    /// Returns true if successful, false if version space is exhausted.
    #[inline]
    pub fn roll(&mut self) -> bool {
        if self.mask == 0 {
            return false;
        }
        if self.current_bits < self.max_bits {
            self.current_bits += 1;
            true
        } else {
            false
        }
    }

    /// Resets the roller to initial state (for new jobs).
    pub fn reset(&mut self) {
        self.current_bits = 0;
    }

    /// Sets a new base version (for new jobs), preserving the mask.
    pub fn set_base_version(&mut self, base_version: i32) {
        self.base_version = base_version & !(self.mask as i32);
        self.reset();
    }

    /// Returns the mask being used.
    pub fn mask(&self) -> u32 {
        self.mask
    }

    /// Returns the number of possible version combinations.
    pub fn combinations(&self) -> u64 {
        if self.mask == 0 {
            1
        } else {
            (self.max_bits as u64) + 1
        }
    }

    /// Returns true if version rolling is enabled.
    pub fn is_enabled(&self) -> bool {
        self.mask != 0
    }

    /// Returns the total nonce space available (nonce * versions).
    pub fn total_nonce_space(&self) -> u64 {
        self.combinations() * (u32::MAX as u64 + 1)
    }

    /// Checks if the given mask has enough bits for effective mining.
    pub fn validate_mask(mask: u32) -> bool {
        mask.count_ones() >= MIN_VERSION_BITS
    }
}

/// Extracts the rolled version bits from a complete version value.
#[inline]
pub fn extract_version_bits(version: i32, mask: u32) -> Option<u32> {
    let bits = (version as u32) & mask;
    if bits == 0 { None } else { Some(bits) }
}

/// Applies version bits to a base version using the given mask.
#[inline]
pub fn apply_version_bits(base_version: i32, bits: u32, mask: u32) -> i32 {
    (base_version & !(mask as i32)) | ((bits & mask) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bip320_mask_has_correct_bits() {
        assert_eq!(BIP320_VERSION_MASK, 0x1FFFE000);
        assert_eq!(BIP320_VERSION_MASK.count_ones(), 16);
        assert_eq!(BIP320_VERSION_MASK.trailing_zeros(), 13);
        assert_eq!(BIP320_VERSION_MASK.leading_zeros(), 3);
    }

    #[test]
    fn new_roller_starts_at_zero() {
        let roller = VersionRoller::with_bip320_mask(0x20000000);
        assert_eq!(roller.current_bits, 0);
        assert_eq!(roller.rolled_bits(), None);
    }

    #[test]
    fn roller_preserves_base_version_non_mask_bits() {
        let base = 0x20000000_i32; // Version 2 with some flags
        let roller = VersionRoller::with_bip320_mask(base);

        // Base version bits outside mask should be preserved
        let version = roller.current_version();
        assert_eq!(
            version & !BIP320_VERSION_MASK as i32,
            base & !BIP320_VERSION_MASK as i32
        );
    }

    #[test]
    fn roller_current_version_includes_rolled_bits() {
        let base = 0x20000000_i32;
        let mut roller = VersionRoller::with_bip320_mask(base);

        assert!(roller.roll());
        let version = roller.current_version();

        // Should have the rolled bit set
        assert_ne!(version, base);
        assert_eq!((version as u32) & BIP320_VERSION_MASK, 1 << 13);
    }

    #[test]
    fn roller_roll_increments_correctly() {
        let mut roller = VersionRoller::with_bip320_mask(0x20000000);

        for expected in 1..=100 {
            assert!(roller.roll());
            assert_eq!(roller.current_bits, expected);
        }
    }

    #[test]
    fn roller_exhausts_after_max_combinations() {
        let mask = 0b11 << 13; // Only 2 bits = 4 combinations (0, 1, 2, 3)
        let mut roller = VersionRoller::new(0x20000000, mask);

        assert_eq!(roller.combinations(), 4);

        // Roll through all combinations
        assert!(roller.roll()); // 1
        assert!(roller.roll()); // 2
        assert!(roller.roll()); // 3
        assert!(!roller.roll()); // exhausted
        assert!(!roller.roll()); // still exhausted
    }

    #[test]
    fn roller_reset_returns_to_zero() {
        let mut roller = VersionRoller::with_bip320_mask(0x20000000);

        for _ in 0..50 {
            roller.roll();
        }
        assert_eq!(roller.current_bits, 50);

        roller.reset();
        assert_eq!(roller.current_bits, 0);
    }

    #[test]
    fn roller_disabled_cannot_roll() {
        let mut roller = VersionRoller::disabled(0x20000000);

        assert!(!roller.is_enabled());
        assert!(!roller.roll());
        assert_eq!(roller.combinations(), 1);
        assert_eq!(roller.current_version(), 0x20000000);
    }

    #[test]
    fn roller_bip320_has_65536_combinations() {
        let roller = VersionRoller::with_bip320_mask(0x20000000);
        assert_eq!(roller.combinations(), 65536);
    }

    #[test]
    fn roller_total_nonce_space_with_bip320() {
        let roller = VersionRoller::with_bip320_mask(0x20000000);
        let expected = 65536_u64 * (u32::MAX as u64 + 1);
        assert_eq!(roller.total_nonce_space(), expected);
    }

    #[test]
    fn roller_set_base_version_resets() {
        let mut roller = VersionRoller::with_bip320_mask(0x20000000);

        for _ in 0..10 {
            roller.roll();
        }

        roller.set_base_version(0x30000000);
        assert_eq!(roller.current_bits, 0);
        assert_eq!(
            roller.current_version() & !BIP320_VERSION_MASK as i32,
            0x30000000 & !BIP320_VERSION_MASK as i32
        );
    }

    #[test]
    fn rolled_bits_returns_none_when_zero() {
        let roller = VersionRoller::with_bip320_mask(0x20000000);
        assert_eq!(roller.rolled_bits(), None);
    }

    #[test]
    fn rolled_bits_returns_shifted_value() {
        let mut roller = VersionRoller::with_bip320_mask(0x20000000);

        roller.roll(); // current_bits = 1
        assert_eq!(roller.rolled_bits(), Some(1 << 13));

        for _ in 0..99 {
            roller.roll();
        }
        // current_bits = 100
        assert_eq!(roller.rolled_bits(), Some(100 << 13));
    }

    #[test]
    fn extract_version_bits_extracts_correctly() {
        let version = 0x20000000_i32 | (42 << 13);
        let bits = extract_version_bits(version, BIP320_VERSION_MASK);
        assert_eq!(bits, Some(42 << 13));
    }

    #[test]
    fn extract_version_bits_returns_none_for_zero() {
        let version = 0x20000000_i32;
        let bits = extract_version_bits(version, BIP320_VERSION_MASK);
        assert_eq!(bits, None);
    }

    #[test]
    fn apply_version_bits_works_correctly() {
        let base = 0x20000000_i32;
        let bits = 42_u32 << 13;
        let result = apply_version_bits(base, bits, BIP320_VERSION_MASK);

        assert_eq!(result & !BIP320_VERSION_MASK as i32, base);
        assert_eq!((result as u32) & BIP320_VERSION_MASK, bits);
    }

    #[test]
    fn validate_mask_requires_min_bits() {
        assert!(!VersionRoller::validate_mask(0));
        assert!(!VersionRoller::validate_mask(0b1 << 13)); // 1 bit
        assert!(VersionRoller::validate_mask(0b11 << 13)); // 2 bits
        assert!(VersionRoller::validate_mask(BIP320_VERSION_MASK)); // 16 bits
    }

    #[test]
    fn version_roller_with_real_bitcoin_version() {
        // Bitcoin version 0x20000000 (BIP 9 VERSIONBITS_TOP_BITS)
        let base = 0x20000000_i32;
        let mut roller = VersionRoller::with_bip320_mask(base);

        // First version should be base unchanged
        assert_eq!(roller.current_version(), base);

        // After rolling, should have mask bits set
        roller.roll();
        let version = roller.current_version();

        // Top bits (31-29) should still be 001
        assert_eq!((version >> 29) & 0b111, 0b001);

        // Bit 28 (inside mask) should now potentially be set
        // Bits 12 and below (outside mask) should be 0
        assert_eq!(version & 0x1FFF, 0);
    }

    #[test]
    fn version_mask_bits_are_contiguous() {
        // BIP 320 mask bits should be contiguous
        let mask = BIP320_VERSION_MASK;
        let shift = mask.trailing_zeros();
        let normalized = mask >> shift;

        // Check all bits from 0 to count_ones are set
        let expected = (1_u32 << mask.count_ones()) - 1;
        assert_eq!(normalized, expected, "Mask bits should be contiguous");
    }

    #[test]
    fn roller_version_sequence_is_deterministic() {
        let base = 0x20000000_i32;
        let mut roller1 = VersionRoller::with_bip320_mask(base);
        let mut roller2 = VersionRoller::with_bip320_mask(base);

        let mut versions1 = Vec::new();
        let mut versions2 = Vec::new();

        for _ in 0..100 {
            versions1.push(roller1.current_version());
            versions2.push(roller2.current_version());
            roller1.roll();
            roller2.roll();
        }

        assert_eq!(versions1, versions2);
    }

    #[test]
    fn roller_all_versions_are_unique() {
        let mut roller = VersionRoller::with_bip320_mask(0x20000000);
        let mut versions = std::collections::HashSet::new();

        versions.insert(roller.current_version());
        for _ in 0..1000 {
            roller.roll();
            let version = roller.current_version();
            assert!(
                versions.insert(version),
                "Duplicate version found: {version:#x}"
            );
        }
    }

    #[test]
    fn roller_custom_mask_calculates_combinations_correctly() {
        // 4-bit mask at position 16
        let mask = 0b1111 << 16;
        let roller = VersionRoller::new(0x20000000, mask);
        assert_eq!(roller.combinations(), 16);

        // 8-bit mask at position 8
        let mask = 0xFF << 8;
        let roller = VersionRoller::new(0x20000000, mask);
        assert_eq!(roller.combinations(), 256);
    }

    #[test]
    fn roller_handles_edge_case_masks() {
        // Single bit mask
        let mask = 1 << 20;
        let mut roller = VersionRoller::new(0x20000000, mask);
        assert_eq!(roller.combinations(), 2);
        assert!(roller.roll());
        assert!(!roller.roll());

        // All valid bits (max practical mask)
        let mask = 0x1FFFFFFF; // 29 bits
        let roller = VersionRoller::new(0, mask);
        assert_eq!(roller.combinations(), 0x20000000);
    }

    #[test]
    fn version_rolling_integration_with_header_version() {
        // Simulate what happens in actual mining
        let job_version = bitcoin::block::Version::from_consensus(0x20000000);
        let base = job_version.to_consensus();

        let mut roller = VersionRoller::with_bip320_mask(base);

        // Simulate rolling through several versions
        for i in 0..10 {
            let version = roller.current_version();
            let reconstructed = bitcoin::block::Version::from_consensus(version);

            // Verify it's a valid version value
            assert_eq!(reconstructed.to_consensus(), version);

            // Verify base bits preserved
            assert_eq!(
                version & !BIP320_VERSION_MASK as i32,
                base & !BIP320_VERSION_MASK as i32,
                "Iteration {i}: base bits should be preserved"
            );

            roller.roll();
        }
    }

    #[test]
    fn stress_test_version_roller_full_cycle() {
        // Use a small mask to test full exhaustion
        let mask = 0b1111 << 13; // 4 bits = 16 combinations
        let mut roller = VersionRoller::new(0x20000000, mask);

        let mut count = 0;
        loop {
            count += 1;
            if !roller.roll() {
                break;
            }
        }

        // Started at 0, rolled to 15, then exhausted
        assert_eq!(count, 16);
        assert_eq!(roller.current_bits, 15);
    }
}
