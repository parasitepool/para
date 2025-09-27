use super::*;

#[derive(Debug)]
pub(crate) struct Hasher {
    pub(crate) extranonce2: Extranonce,
    pub(crate) header: Header,
    pub(crate) job_id: String,
    pub(crate) pool_target: Target,
    last_best_hash: Option<u64>,
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
    pub(crate) fn new(
        header: Header,
        pool_target: Target,
        extranonce2: Extranonce,
        job_id: String,
    ) -> Self {
        Self {
            header,
            pool_target,
            extranonce2,
            job_id,
            last_best_hash: None,
        }
    }

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

        let base_header = self.header;
        let pool_target = self.pool_target;
        let cancel_clone = cancel.clone();

        let progress_state = Arc::clone(&mining_state);
        let progress_cancel = cancel.clone();
        let progress_handle = tokio::spawn(async move {
            let mut last_hashes = 0u64;
            let mut last_time = start;
            let mut ewma_hashrate: Option<f64> = None;

            const ALPHA: f64 = 0.18;

            while !progress_cancel.is_cancelled()
                && !progress_state.found_solution.load(Ordering::Relaxed)
            {
                tokio::time::sleep(Duration::from_secs(5)).await;

                let current_hashes = progress_state.total_hashes.load(Ordering::Relaxed);
                let active_threads = progress_state.active_threads.load(Ordering::Relaxed);
                let now = Instant::now();
                let elapsed = now.duration_since(last_time).as_secs_f64().max(1e-6);

                let recent_hashes = current_hashes.saturating_sub(last_hashes);
                let current_period_hashrate = recent_hashes as f64 / elapsed;

                ewma_hashrate = Some(match ewma_hashrate {
                    Some(prev_ewma) => ALPHA * current_period_hashrate + (1.0 - ALPHA) * prev_ewma,
                    None => current_period_hashrate,
                });

                let smoothed_hashrate = ewma_hashrate.unwrap_or(0.0);

                if active_threads > 0 || current_hashes > last_hashes {
                    info!(
                        avg_hashrate = %HashRate(smoothed_hashrate),
                        current_hashrate = %HashRate(current_period_hashrate),
                        active_threads = active_threads,
                        total_hashes = current_hashes,
                        "Mining progress"
                    );
                } else if current_hashes == 0 && active_threads == 0 {
                    info!(
                        hashrate = %HashRate(smoothed_hashrate),
                        threads = active_threads,
                        hashes = current_hashes,
                        "Mining status"
                    );
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

        progress_handle.abort();

        let duration = start.elapsed();
        self.log_mining_summary(&mining_state, duration);

        self.last_best_hash = Some(mining_state.best_hash.load(Ordering::Relaxed));

        if cancel.is_cancelled() {
            return Err(anyhow!("hasher cancelled"));
        }

        if mining_state.found_solution.load(Ordering::Relaxed) {
            let solution_nonce = mining_state.solution_nonce.load(Ordering::Relaxed);
            let active_threads = mining_state.active_threads.load(Ordering::Relaxed);
            let total_hashes = mining_state.total_hashes.load(Ordering::Relaxed);

            let solution_stats = MiningStats {
                hashrate: 0.0,
                active_threads,
                total_hashes,
            };

            let mut solution_header = self.header;
            solution_header.nonce = solution_nonce;
            let hash = solution_header.block_hash();

            info!(
                nonce = solution_header.nonce,
                %hash,
                %solution_stats,
                "Solution found"
            );

            return Ok((
                solution_header,
                self.extranonce2.clone(),
                self.job_id.clone(),
            ));
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

                    info!(nonce = nonce, "Thread found solution at nonce");
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

    fn log_mining_summary(&self, mining_state: &Arc<MiningState>, duration: Duration) {
        let total_hashes = mining_state.total_hashes.load(Ordering::Relaxed);
        let final_active_threads = mining_state.active_threads.load(Ordering::Relaxed);
        let avg_hashrate = total_hashes as f64 / duration.as_secs_f64().max(1e-6);

        info!(
            total_hashes = total_hashes,
            duration_secs = %format!("{:.2}", duration.as_secs_f64()),
            avg_hashrate = %HashRate(avg_hashrate),
            final_active_threads = final_active_threads,
            "Mining completed"
        );

        if final_active_threads > 0 {
            warn!(
                active_threads = final_active_threads,
                "Warning: threads still marked as active after mining completion"
            );
        }
    }
}

#[derive(Debug)]
pub struct HashRate(pub f64);

impl std::fmt::Display for HashRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rate = self.0;

        if rate >= 1_000_000_000_000.0 {
            write!(f, "{:.2} TH/s", rate / 1_000_000_000_000.0)
        } else if rate >= 1_000_000_000.0 {
            write!(f, "{:.2} GH/s", rate / 1_000_000_000.0)
        } else if rate >= 1_000_000.0 {
            write!(f, "{:.2} MH/s", rate / 1_000_000.0)
        } else if rate >= 1_000.0 {
            write!(f, "{:.2} KH/s", rate / 1_000.0)
        } else {
            write!(f, "{:.2} H/s", rate)
        }
    }
}

#[derive(Debug)]
pub struct MiningStats {
    pub hashrate: f64,
    pub active_threads: u32,
    pub total_hashes: u64,
}

impl std::fmt::Display for MiningStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} | {} threads | {} hashes",
            HashRate(self.hashrate),
            self.active_threads,
            self.total_hashes
        )
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

    #[tokio::test]
    async fn test_parallel_mining_easy_target() {
        let target = shift(1);
        let mut hasher = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            "parallel_test".into(),
        );

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
        let mut hasher = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            "cancel_test".into(),
        );

        let cancel_token = CancellationToken::new();

        cancel_token.cancel();

        let result = hasher.hash_with_range(cancel_token, 0, u32::MAX);
        assert!(result.is_err(), "Should be cancelled");
        assert!(result.unwrap_err().to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn test_best_hash_tracking() {
        let target = shift(32);
        let mut hasher = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            "best_hash_test".into(),
        );

        assert_eq!(hasher.last_best_hash, None);

        let result = hasher.hash_with_range(CancellationToken::new(), 0, 1000);

        assert!(
            result.is_err(),
            "Should not find solution with impossible target"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("nonce range exhausted")
        );

        let best_hash = hasher.last_best_hash;
        assert!(best_hash.is_some(), "Should have recorded a best hash");
        assert!(
            best_hash.unwrap() < u64::MAX,
            "Best hash should be better than initial value"
        );

        let first_best = best_hash.unwrap();
        let result2 = hasher.hash_with_range(CancellationToken::new(), 1000, 2000);

        assert!(
            result2.is_err(),
            "Should not find solution with impossible target"
        );

        let second_best = hasher.last_best_hash.unwrap();
        assert!(
            second_best < u64::MAX,
            "Second mining operation should track some best hash: {}",
            second_best
        );

        assert!(
            first_best < u64::MAX && second_best < u64::MAX,
            "Both mining operations should have found hashes better than initial: {} and {}",
            first_best,
            second_best
        );
    }

    #[tokio::test]
    async fn test_tiny_nonce_ranges() {
        let target = shift(4);

        let mut hasher = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            "tiny_range_1".into(),
        );
        let result = hasher.hash_with_range(CancellationToken::new(), 0, 1);
        assert!(
            result.is_ok()
                || result
                    .unwrap_err()
                    .to_string()
                    .contains("nonce range exhausted")
        );

        let mut hasher2 = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            "tiny_range_5".into(),
        );
        let result2 = hasher2.hash_with_range(CancellationToken::new(), 100, 105);
        assert!(
            result2.is_ok()
                || result2
                    .unwrap_err()
                    .to_string()
                    .contains("nonce range exhausted")
        );

        let mut hasher3 = Hasher::new(
            header(None, None),
            target,
            "0000000000".parse().unwrap(),
            "tiny_range_10".into(),
        );
        let result3 = hasher3.hash_with_range(CancellationToken::new(), 0, 10);
        assert!(
            result3.is_ok()
                || result3
                    .unwrap_err()
                    .to_string()
                    .contains("nonce range exhausted")
        );

        assert!(
            hasher3.last_best_hash.is_some(),
            "Should track best hash even in tiny ranges"
        );
        assert!(
            hasher3.last_best_hash.unwrap() < u64::MAX,
            "Should have found some hash better than initial"
        );
    }

    #[test]
    fn test_hashrate_display() {
        assert_eq!(format!("{}", HashRate(1500.0)), "1.50 KH/s");
        assert_eq!(format!("{}", HashRate(2_500_000.0)), "2.50 MH/s");
        assert_eq!(format!("{}", HashRate(3_200_000_000.0)), "3.20 GH/s");
        assert_eq!(format!("{}", HashRate(1_100_000_000_000.0)), "1.10 TH/s");
    }
}
