use super::*;
use std::io::{self, Write};

#[derive(Debug)]
pub(crate) struct Hasher {
    pub(crate) extranonce2: String,
    pub(crate) header: Header,
    pub(crate) job_id: String,
    pub(crate) pool_target: Target,
}

impl Hasher {
    pub(crate) fn hash(&mut self, cancel: CancellationToken) -> Result<(Header, String, String)> {
        const CANCEL_CHECK_INTERVAL: u32 = 1000;

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

// Creates a target with specified difficulty (number of leading zero bits)
//
// # Arguments
// * `difficulty` - Number of leading zero bits required (0-32)
//
// # Returns
// A `Target` that requires `difficulty` leading zero bits
//
// # Panics
// Panics if difficulty > 32
#[allow(dead_code)]
fn target_with_difficulty(difficulty: u8) -> Target {
    assert!(difficulty <= 32, "difficulty too high");

    let mut bytes = [0xFFu8; 32];

    let full_zero_bytes = (difficulty / 8) as usize;
    let partial_bits = difficulty % 8;

    // Fix: Use iterator instead of range loop
    for byte in bytes.iter_mut().take(full_zero_bytes) {
        *byte = 0x00;
    }

    if partial_bits > 0 {
        // Mask with 'partial_bits' leading zeros, rest ones.
        // E.g. for partial_bits = 4, mask = 0xFF >> 4 = 00001111 (0x0F)
        let mask = 0xFF >> partial_bits;
        bytes[full_zero_bytes] = mask;
    }

    Target::from_be_bytes(bytes)
}

// Prompts user for difficulty selection and returns the corresponding target
// Need to build seperate .rs for interactive testing
fn read_line_trimmed() -> Result<String> {
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

#[allow(dead_code)]
fn get_difficulty_from_user() -> Result<Target> {
    const DIFFICULTIES: &[(u8, &str)] = &[
        (1, "Very Easy (1 bit difficulty)"),
        (4, "Easy (4 bits difficulty)"),
        (8, "Medium (8 bits difficulty)"),
        (12, "Hard (12 bits difficulty)"),
        (16, "Very Hard (16 bits difficulty)"),
        (20, "Extreme (20 bits difficulty)"),
    ];

    println!("Select mining difficulty:");
    for (i, &(_, desc)) in DIFFICULTIES.iter().enumerate() {
        println!("{}. {}", i + 1, desc);
    }
    println!("7. Custom difficulty");

    loop {
        print!("Enter your choice (1-7): ");
        io::stdout().flush()?;

        match read_line_trimmed()?.parse::<u8>() {
            Ok(choice @ 1..=6) => {
                let difficulty = DIFFICULTIES[(choice - 1) as usize].0;
                println!("Selected difficulty: {difficulty} bits");
                return Ok(target_with_difficulty(difficulty));
            }
            Ok(7) => loop {
                print!("Enter custom difficulty (1-32): ");
                io::stdout().flush()?;
                let input = read_line_trimmed()?;
                if let Ok(custom_diff) = input.parse::<u8>() {
                    if (1..=32).contains(&custom_diff) {
                        println!("Selected custom difficulty: {custom_diff} bits");
                        return Ok(target_with_difficulty(custom_diff));
                    }
                }
                println!("Invalid custom difficulty. Please try again.");
            },
            _ => println!("Invalid choice. Please enter a number between 1 and 7."),
        }
    }
}

// Formats hashrate with appropriate units (E/P/T/G/M/K/H)
#[allow(dead_code)]
fn format_hashrate(hashes_per_second: f64) -> String {
    if hashes_per_second <= 0.0 {
        return "0 H/s".into();
    }

    const UNITS: &[(f64, &str)] = &[
        (1e18, "EH/s"),
        (1e15, "PH/s"),
        (1e12, "TH/s"),
        (1e9, "GH/s"),
        (1e6, "MH/s"),
        (1e3, "KH/s"),
    ];

    for &(threshold, suffix) in UNITS {
        if hashes_per_second >= threshold {
            let val = hashes_per_second / threshold;
            return format!("{val:.2} {suffix}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string();
        }
    }
    format!("{hashes_per_second:.2} H/s")
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
        // Use difficulty 1 instead of Target::MAX for quick testing
        let target = target_with_difficulty(1);
        let mut hasher = Hasher {
            header: header(None, None), // Don't set network target in bits field
            pool_target: target,
            extranonce2: "00000000000".into(),
            job_id: "bf".into(),
        };

        let (header, _extranonce2, _job_id) = hasher.hash(CancellationToken::new()).unwrap();
        // Use pool_target for validation instead of the header's bits field
        assert!(target.is_met_by(header.block_hash()));
    }

    #[test]
    fn hasher_nonce_space_exhausted() {
        // Use higher difficulty to ensure miner is able to hit nonce exhaustion
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
        // Fix: Use iterator instead of range loop
        for byte in hard_bytes.iter().take(4) {
            assert_eq!(*byte, 0);
        }
        assert_eq!(hard_bytes[4], 0xFF);
    }

    #[test]
    fn test_hashrate_formatting() {
        assert_eq!(format_hashrate(1000.0), "1.00 KH/s");
        assert_eq!(format_hashrate(1_000_000.0), "1.00 MH/s");
        assert_eq!(format_hashrate(1_000_000_000.0), "1.00 GH/s");
        assert_eq!(format_hashrate(1_000_000_000_000.0), "1.00 TH/s");
        assert_eq!(format_hashrate(1_000_000_000_000_000.0), "1.00 PH/s");
        assert_eq!(format_hashrate(1_000_000_000_000_000_000.0), "1.00 EH/s");
    }

    #[test]
    fn test_difficulty_progression() {
        // Test that difficulties create properly ordered targets
        let difficulties = [1, 4, 8, 12, 16, 20, 24];
        let mut targets = Vec::new();

        for &diff in &difficulties {
            targets.push(target_with_difficulty(diff));
        }

        // Each target should be smaller (more restrictive) than the previous
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
        // Test that hasher works with different difficulty levels
        let difficulties = [1, 2, 3, 4]; // Keep low for test speed

        for difficulty in difficulties {
            let target = target_with_difficulty(difficulty);
            let mut hasher = Hasher {
                header: header(None, None), // Again, don't set network target in bits field
                pool_target: target,
                extranonce2: "00000000000".into(),
                job_id: format!("test_{difficulty}"),
            };

            let result = hasher.hash(CancellationToken::new());
            assert!(result.is_ok(), "Failed at difficulty {difficulty}");

            let (header, _, _) = result.unwrap();
            // Use pool_target for validation instead of validate_pow
            assert!(
                target.is_met_by(header.block_hash()),
                "Invalid PoW at difficulty {difficulty}"
            );
        }
    }
}
