use super::*;

#[derive(Debug)]
pub(crate) struct Hasher {
    pub(crate) extranonce2: Extranonce,
    pub(crate) header: Header,
    pub(crate) job_id: String,
    pub(crate) pool_target: Target,
}

struct MiningState {
    found_solution: AtomicBool,
    total_hashes: AtomicU64,
    solution_nonce: AtomicU32,
    solution_header: parking_lot::Mutex<Option<Header>>,
    best_hash: AtomicU64,
    active_threads: AtomicU32,
}

impl Hasher {
    pub(crate) fn hash_with_range(
        &mut self,
        cancel: CancellationToken,
        start_nonce: u32,
        end_nonce: u32,
    ) -> Result<(Header, Extranonce, String)> {
        let span =
            tracing::info_span!("hasher", job_id = %self.job_id, extranonce2 = %self.extranonce2);
        let _guard = span.enter();

        let start = Instant::now();
        let mining_state = Arc::new(MiningState {
            found_solution: AtomicBool::new(false),
            total_hashes: AtomicU64::new(0),
            solution_nonce: AtomicU32::new(0),
            solution_header: parking_lot::Mutex::new(None),
            best_hash: AtomicU64::new(u64::MAX),
            active_threads: AtomicU32::new(0),
        });

        let chunk_size = crate::subcommand::miner::mining_utils::calculate_optimal_chunk_size();
        const PROGRESS_INTERVAL: u64 = 1_000_000;

        let base_header = self.header;
        let pool_target = self.pool_target;
        let cancel_clone = cancel.clone();

        // Spawn progress monitor using std::thread instead of tokio
        let progress_state = Arc::clone(&mining_state);
        let progress_cancel = cancel.clone();
        let progress_handle = std::thread::spawn(move || {
            let mut last_hashes = 0u64;
            let mut last_time = start;

            while !progress_cancel.is_cancelled()
                && !progress_state.found_solution.load(Ordering::Relaxed)
            {
                std::thread::sleep(Duration::from_secs(5));

                let current_hashes = progress_state.total_hashes.load(Ordering::Relaxed);
                let now = Instant::now();
                let elapsed = now.duration_since(last_time).as_secs_f64().max(1e-6);

                if current_hashes > last_hashes
                    && (current_hashes - last_hashes) >= PROGRESS_INTERVAL
                {
                    let recent_hashes = current_hashes - last_hashes;
                    let current_hashrate = recent_hashes as f64 / elapsed;
                    info!("Hashrate: {}", HashRate(current_hashrate));
                }

                last_hashes = current_hashes;
                last_time = now;
            }
        });

        if start_nonce >= end_nonce {
            return Err(anyhow!(
                "invalid nonce range: {} >= {}",
                start_nonce,
                end_nonce
            ));
        }

        let nonce_range = start_nonce..end_nonce;
        let chunks: Vec<u32> = nonce_range.step_by(chunk_size as usize).collect();

        chunks.par_iter().find_any(|&&chunk_start| {
            if cancel_clone.is_cancelled() || mining_state.found_solution.load(Ordering::Relaxed) {
                return false;
            }

            let chunk_end = std::cmp::min(chunk_start.saturating_add(chunk_size), end_nonce);

            self.process_nonce_chunk(
                chunk_start,
                chunk_end,
                base_header,
                pool_target,
                &mining_state,
                &cancel_clone,
            )
        });

        // Signal progress thread to stop and wait for it
        drop(progress_handle);

        if cancel.is_cancelled() {
            return Err(anyhow!("hasher cancelled"));
        }

        if mining_state.found_solution.load(Ordering::Relaxed) {
            let solution_nonce = mining_state.solution_nonce.load(Ordering::Relaxed);
            let solution_header = mining_state.solution_header.lock().take();

            if let Some(mut header) = solution_header {
                header.nonce = solution_nonce;
                let hash = header.block_hash();
                info!("Solution found: nonce={}, hash={:?}", header.nonce, hash);
                return Ok((header, self.extranonce2.clone(), self.job_id.clone()));
            } else {
                let mut final_header = self.header;
                final_header.nonce = solution_nonce;
                let hash = final_header.block_hash();
                info!(
                    "Solution found: nonce={}, hash={:?}",
                    final_header.nonce, hash
                );
                return Ok((final_header, self.extranonce2.clone(), self.job_id.clone()));
            }
        }

        Err(anyhow!(
            "nonce range exhausted: {}-{}",
            start_nonce,
            end_nonce
        ))
    }

    fn process_nonce_chunk(
        &self,
        start_nonce: u32,
        end_nonce: u32,
        mut header: Header,
        pool_target: Target,
        mining_state: &Arc<MiningState>,
        cancel: &CancellationToken,
    ) -> bool {
        mining_state.active_threads.fetch_add(1, Ordering::Relaxed);

        let mut local_hashes = 0u64;
        let mut best_local_hash = u64::MAX;

        for nonce in start_nonce..end_nonce {
            if local_hashes.is_multiple_of(10000)
                && (cancel.is_cancelled() || mining_state.found_solution.load(Ordering::Relaxed))
            {
                mining_state
                    .total_hashes
                    .fetch_add(local_hashes, Ordering::Relaxed);
                mining_state.active_threads.fetch_sub(1, Ordering::Relaxed);
                return false;
            }

            header.nonce = nonce;
            let hash = header.block_hash();
            local_hashes += 1;

            let hash_u64 = self.hash_to_u64(&hash);
            if hash_u64 < best_local_hash {
                best_local_hash = hash_u64;

                let mut current_best = mining_state.best_hash.load(Ordering::Relaxed);
                while hash_u64 < current_best {
                    match mining_state.best_hash.compare_exchange_weak(
                        current_best,
                        hash_u64,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(new_current) => current_best = new_current,
                    }
                }
            }

            if pool_target.is_met_by(hash) {
                if mining_state
                    .found_solution
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
                    .is_ok()
                {
                    mining_state.solution_nonce.store(nonce, Ordering::Relaxed);
                    *mining_state.solution_header.lock() = Some(header);
                    mining_state
                        .total_hashes
                        .fetch_add(local_hashes, Ordering::Relaxed);

                    info!("Thread found solution at nonce: {}", nonce);
                    mining_state.active_threads.fetch_sub(1, Ordering::Relaxed);
                    return true;
                }
                break;
            }
        }

        mining_state
            .total_hashes
            .fetch_add(local_hashes, Ordering::Relaxed);
        mining_state.active_threads.fetch_sub(1, Ordering::Relaxed);
        false
    }

    fn hash_to_u64(&self, hash: &bitcoin::BlockHash) -> u64 {
        let bytes = hash.as_byte_array();
        u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])
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

    #[tokio::test]
    async fn hasher_hashes_with_very_low_leading_zeros() {
        let target = shift(1);
        let mut hasher = Hasher {
            header: header(None, None),
            pool_target: target,
            extranonce2: "0000000000".parse().unwrap(),
            job_id: "bf".into(),
        };

        let (header, _extranonce2, _job_id) = hasher
            .hash_with_range(CancellationToken::new(), 0, 1_000_000)
            .unwrap();
        assert!(target.is_met_by(header.block_hash()));
    }

    #[tokio::test]
    async fn hasher_nonce_space_exhausted() {
        let target = shift(32);
        let mut hasher = Hasher {
            header: header(None, Some(u32::MAX - 1)),
            pool_target: target,
            extranonce2: "0000000000".parse().unwrap(),
            job_id: "bg".into(),
        };

        let result = hasher.hash_with_range(CancellationToken::new(), u32::MAX - 1, u32::MAX);
        assert!(
            result.is_err_and(|err| err.to_string() == "nonce range exhausted: 4294967294-4294967295")
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

    #[tokio::test]
    async fn test_multiple_leading_zeros_levels() {
        let leading_zeros = [1, 2, 3, 4];

        for zeros in leading_zeros {
            let target = shift(zeros);
            let mut hasher = Hasher {
                header: header(None, None),
                pool_target: target,
                extranonce2: "0000000000".parse().unwrap(),
                job_id: format!("test_{zeros}"),
            };

            let result = hasher.hash_with_range(CancellationToken::new(), 0, 10_000_000);
            assert!(result.is_ok(), "Failed at {zeros} leading zeros");

            let (header, _, _) = result.unwrap();
            assert!(
                target.is_met_by(header.block_hash()),
                "Invalid PoW at {zeros} leading zeros"
            );
        }
    }

    #[tokio::test]
    async fn test_parallel_mining_easy_target() {
        let target = shift(1);
        let mut hasher = Hasher {
            header: header(None, None),
            pool_target: target,
            extranonce2: "0000000000".parse().unwrap(),
            job_id: "parallel_test".into(),
        };

        let result = hasher.hash_with_range(CancellationToken::new(), 0, 100000);
        assert!(
            result.is_ok(),
            "Parallel mining should find solution for easy target"
        );

        let (header, _, _) = result.unwrap();
        assert!(
            target.is_met_by(header.block_hash()),
            "Solution should meet target"
        );
    }

    #[tokio::test]
    async fn test_parallel_mining_cancellation() {
        let target = shift(30);
        let mut hasher = Hasher {
            header: header(None, None),
            pool_target: target,
            extranonce2: "0000000000".parse().unwrap(),
            job_id: "cancel_test".into(),
        };

        let cancel_token = CancellationToken::new();

        cancel_token.cancel();

        let result = hasher.hash_with_range(cancel_token, 0, u32::MAX);
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
