use {
    super::*,
    crate::stratum::{Client, ClientConfig, Event, MerkleNode, Notify, SubscribeResult},
    anyhow::Context,
    bitcoin::{
        Address, Network, Transaction,
        consensus::Decodable,
        hashes::{Hash, sha256d},
    },
    std::io::Cursor,
};

#[derive(Debug, Parser)]
pub struct Template {
    #[arg(help = "Stratum <HOST:PORT>.")]
    stratum_endpoint: String,
    #[arg(long, help = "Stratum <USERNAME>.")]
    pub username: String,
    #[arg(long, help = "Stratum <PASSWORD>.")]
    pub password: Option<String>,
    #[arg(long, help = "Continue watching for template updates.")]
    pub watch: bool,
    #[arg(
        long,
        help = "Show raw mining.notify JSON array as received from protocol."
    )]
    pub raw: bool,
    #[arg(
        long,
        default_value = "bitcoin",
        help = "Network for address encoding (bitcoin, testnet, signet, regtest)."
    )]
    pub network: Network,
    #[arg(
        long,
        help = "Only show significant events (new blocks, difficulty changes). Implies --watch."
    )]
    pub quiet: bool,
}

/// Interpreted block template output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretedOutput {
    pub job_id: JobId,
    pub prevhash: String,
    pub merkle_root: String,
    pub coinbase: CoinbaseInfo,
    pub merkle_branches: Vec<String>,
    pub version: String,
    pub version_info: VersionInfo,
    pub nbits: String,
    pub ntime: String,
    pub ntime_human: String,
    pub difficulty: f64,
    pub clean_jobs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub bits: u32,
    pub bip9_signaling: bool,
    pub version_rolling_possible: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub signaled_bits: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinbaseInfo {
    pub size_bytes: usize,
    pub input_text: Option<String>,
    pub outputs: Vec<CoinbaseOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinbaseOutput {
    pub value_sats: u64,
    pub value_btc: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// Template change event for watch mode
#[derive(Debug, Serialize)]
pub struct TemplateChange {
    pub event: ChangeEvent,
    pub job_id: JobId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prevhash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ntime_human: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clean_jobs: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeEvent {
    NewBlock,
    DifficultyChange,
    TemplateUpdate,
}

/// Statistics tracked during watch mode
#[derive(Debug, Default)]
struct WatchStats {
    templates_received: u64,
    blocks_seen: u64,
    start_time: Option<std::time::Instant>,
    last_template_time: Option<std::time::Instant>,
    total_interval_ms: u64,
    // Difficulty tracking
    difficulty_changes: u64,
    min_difficulty: Option<f64>,
    max_difficulty: Option<f64>,
    last_difficulty: Option<f64>,
}

impl WatchStats {
    fn record_template(&mut self, is_new_block: bool, difficulty: Option<f64>) {
        let now = std::time::Instant::now();

        if self.start_time.is_none() {
            self.start_time = Some(now);
        }

        if let Some(last) = self.last_template_time {
            self.total_interval_ms += now.duration_since(last).as_millis() as u64;
        }

        self.templates_received += 1;
        if is_new_block {
            self.blocks_seen += 1;
        }
        self.last_template_time = Some(now);

        // Track difficulty
        if let Some(diff) = difficulty {
            if let Some(last_diff) = self.last_difficulty
                && (diff - last_diff).abs() > f64::EPSILON
            {
                self.difficulty_changes += 1;
            }

            self.min_difficulty = Some(self.min_difficulty.map(|m| m.min(diff)).unwrap_or(diff));
            self.max_difficulty = Some(self.max_difficulty.map(|m| m.max(diff)).unwrap_or(diff));
            self.last_difficulty = Some(diff);
        }
    }

    fn summary(&self) -> String {
        let elapsed = self.start_time.map(|s| s.elapsed().as_secs()).unwrap_or(0);

        let avg_interval = if self.templates_received > 1 {
            self.total_interval_ms / (self.templates_received - 1)
        } else {
            0
        };

        let diff_info = match (self.min_difficulty, self.max_difficulty) {
            (Some(min), Some(max)) if (max - min).abs() > f64::EPSILON => {
                format!(
                    ", difficulty range {:.2e}-{:.2e} ({} changes)",
                    min, max, self.difficulty_changes
                )
            }
            (Some(d), _) => format!(", difficulty {:.2e}", d),
            _ => String::new(),
        };

        format!(
            "Watch stats: {} templates, {} blocks, avg interval {}ms, elapsed {}s{}",
            self.templates_received, self.blocks_seen, avg_interval, elapsed, diff_info
        )
    }
}

impl Template {
    pub async fn run(self, cancel_token: CancellationToken) -> anyhow::Result<()> {
        info!(
            "Connecting to {} with user {}",
            self.stratum_endpoint, self.username
        );

        let address = resolve_stratum_endpoint(&self.stratum_endpoint)
            .await
            .with_context(|| format!("Failed to resolve endpoint '{}'", self.stratum_endpoint))?;

        let config = ClientConfig {
            address: address.to_string(),
            username: self.username.clone(),
            user_agent: USER_AGENT.into(),
            password: self.password.clone(),
            timeout: Duration::from_secs(5),
        };

        let client = Client::new(config);
        let mut events = client
            .connect()
            .await
            .with_context(|| format!("Failed to connect to {}", address))?;

        let (subscription, _, _) = client
            .subscribe()
            .await
            .context("Stratum subscribe failed")?;

        client
            .authorize()
            .await
            .with_context(|| format!("Authorization failed for user '{}'", self.username))?;

        // Track previous state for change detection
        let mut prev_output: Option<InterpretedOutput> = None;

        // --quiet implies --watch
        let watch = self.watch || self.quiet;

        // Track stats in watch mode
        let mut stats = WatchStats::default();

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    if watch {
                        info!("{}", stats.summary());
                    }
                    info!("Shutting down template monitor");
                    break;
                }
                event = events.recv() => {
                    match event {
                        Ok(Event::Notify(notify)) => {
                            if self.raw {
                                // Print raw mining.notify as JSON array (protocol format)
                                println!("{}", serde_json::to_string_pretty(&notify)?);
                                if watch {
                                    stats.record_template(false, None);
                                }
                            } else {
                                let output = self.interpret_template(
                                    &subscription,
                                    &notify,
                                )?;

                                let is_new_block = prev_output
                                    .as_ref()
                                    .map(|p| p.prevhash != output.prevhash)
                                    .unwrap_or(false);

                                if watch && prev_output.is_some() {
                                    // Show changes instead of full output
                                    if let Some(change) = self.detect_change(&prev_output, &output) {
                                        // In quiet mode, only show significant events
                                        if !self.quiet || self.is_significant(&change) {
                                            println!("{}", serde_json::to_string_pretty(&change)?);
                                        }
                                    }
                                } else {
                                    // First template or not in watch mode - show full output
                                    println!("{}", serde_json::to_string_pretty(&output)?);
                                }

                                if watch {
                                    stats.record_template(is_new_block, Some(output.difficulty));
                                }

                                prev_output = Some(output);
                            }

                            if !watch {
                                break;
                            }
                        }
                        Ok(Event::Disconnected) => {
                            if watch {
                                info!("{}", stats.summary());
                            }
                            error!("Disconnected from stratum server");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }

    fn detect_change(
        &self,
        prev: &Option<InterpretedOutput>,
        curr: &InterpretedOutput,
    ) -> Option<TemplateChange> {
        let prev = prev.as_ref()?;

        // No change
        if prev.job_id == curr.job_id {
            return None;
        }

        let new_block = prev.prevhash != curr.prevhash;
        let diff_change = (prev.difficulty - curr.difficulty).abs() > f64::EPSILON;

        let event = if new_block {
            ChangeEvent::NewBlock
        } else if diff_change {
            ChangeEvent::DifficultyChange
        } else {
            ChangeEvent::TemplateUpdate
        };

        Some(TemplateChange {
            event,
            job_id: curr.job_id,
            prevhash: if new_block {
                Some(curr.prevhash.clone())
            } else {
                None
            },
            difficulty: if diff_change {
                Some(curr.difficulty)
            } else {
                None
            },
            ntime_human: Some(curr.ntime_human.clone()),
            clean_jobs: if curr.clean_jobs { Some(true) } else { None },
        })
    }

    fn is_significant(&self, change: &TemplateChange) -> bool {
        matches!(
            change.event,
            ChangeEvent::NewBlock | ChangeEvent::DifficultyChange
        )
    }

    fn interpret_template(
        &self,
        subscription: &SubscribeResult,
        notify: &Notify,
    ) -> anyhow::Result<InterpretedOutput> {
        // Build full coinbase transaction
        let extranonce2_placeholder = "00".repeat(subscription.extranonce2_size);
        let coinbase_hex = format!(
            "{}{}{}{}",
            notify.coinb1, subscription.extranonce1, extranonce2_placeholder, notify.coinb2
        );

        let coinbase_bytes = hex::decode(&coinbase_hex)?;
        let coinbase_tx = Transaction::consensus_decode(&mut Cursor::new(&coinbase_bytes))?;

        // Extract ASCII text from coinbase input
        let input_text = Self::extract_coinbase_text(&coinbase_tx);

        // Extract outputs with addresses
        let outputs: Vec<CoinbaseOutput> = coinbase_tx
            .output
            .iter()
            .map(|out| {
                let sats = out.value.to_sat();
                let address = Address::from_script(&out.script_pubkey, self.network)
                    .ok()
                    .map(|a| a.to_string());
                CoinbaseOutput {
                    value_sats: sats,
                    value_btc: sats as f64 / 100_000_000.0,
                    address,
                }
            })
            .collect();

        // Calculate difficulty
        let (difficulty, _) = Self::calculate_difficulty_and_target(notify.nbits);

        // Parse ntime for human readable
        let ntime_str = notify.ntime.to_string();
        let ntime_u64 = u64::from_str_radix(&ntime_str, 16).unwrap_or(0);

        // Compute merkle root
        let merkle_root = Self::compute_merkle_root(&coinbase_bytes, &notify.merkle_branches)?;

        // Parse version info (ASICBoost/version rolling detection)
        let version_str = notify.version.to_string();
        let version_u32 = u32::from_str_radix(&version_str, 16).unwrap_or(0);
        let version_info = Self::parse_version_info(version_u32);

        Ok(InterpretedOutput {
            job_id: notify.job_id,
            prevhash: notify.prevhash.to_string(),
            merkle_root,
            coinbase: CoinbaseInfo {
                size_bytes: coinbase_bytes.len(),
                input_text,
                outputs,
            },
            merkle_branches: notify
                .merkle_branches
                .iter()
                .map(|m| m.to_string())
                .collect(),
            version: notify.version.to_string(),
            version_info,
            nbits: notify.nbits.to_string(),
            ntime: ntime_str,
            ntime_human: Self::format_timestamp(ntime_u64),
            difficulty,
            clean_jobs: notify.clean_jobs,
        })
    }

    fn extract_coinbase_text(tx: &Transaction) -> Option<String> {
        if tx.input.is_empty() {
            return None;
        }

        let script_sig = &tx.input[0].script_sig;
        let bytes = script_sig.as_bytes();

        // Extract ASCII strings from the scriptSig (skip first 4 bytes - height)
        let mut ascii_parts: Vec<String> = Vec::new();
        let mut current_string = String::new();

        for &byte in bytes.iter().skip(4) {
            if (0x20..=0x7e).contains(&byte) {
                current_string.push(byte as char);
            } else if !current_string.is_empty() {
                if current_string.len() >= 3 {
                    ascii_parts.push(current_string.clone());
                }
                current_string.clear();
            }
        }
        if current_string.len() >= 3 {
            ascii_parts.push(current_string);
        }

        if ascii_parts.is_empty() {
            None
        } else {
            Some(ascii_parts.join(" "))
        }
    }

    fn calculate_difficulty_and_target(nbits: Nbits) -> (f64, String) {
        let difficulty = crate::stratum::Difficulty::from(nbits);
        let diff_float = difficulty.as_f64();
        let target_bytes = difficulty.to_target().to_be_bytes();
        let target_hex = hex::encode(target_bytes);
        (diff_float, target_hex)
    }

    fn format_timestamp(unix_time: u64) -> String {
        chrono::DateTime::from_timestamp(unix_time as i64, 0)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_else(|| unix_time.to_string())
    }

    fn compute_merkle_root(
        coinbase_bytes: &[u8],
        merkle_branches: &[MerkleNode],
    ) -> anyhow::Result<String> {
        // Hash the coinbase transaction (double SHA256)
        let mut current_hash = sha256d::Hash::hash(coinbase_bytes).to_byte_array();

        // For each merkle branch, concatenate and hash
        for branch in merkle_branches {
            let branch_bytes =
                hex::decode(branch.to_string()).context("Invalid merkle branch hex")?;
            let mut combined = Vec::with_capacity(64);
            combined.extend_from_slice(&current_hash);
            combined.extend_from_slice(&branch_bytes);
            current_hash = sha256d::Hash::hash(&combined).to_byte_array();
        }

        // Return as hex (reversed for display - little endian to big endian)
        current_hash.reverse();
        Ok(hex::encode(current_hash))
    }

    fn parse_version_info(version: u32) -> VersionInfo {
        // BIP9 uses top 3 bits as 001 for signaling
        let bip9_signaling = (version >> 29) == 0b001;

        // BIP320: bits 13-28 can be used for version rolling (ASICBoost)
        // Mask: 0x1FFFE000 (bits 13-28)
        let version_rolling_mask: u32 = 0x1FFFE000;
        let rolling_bits_set = version & version_rolling_mask;
        let version_rolling_possible = rolling_bits_set != 0;

        // Extract signaled BIP9 bits (bits 0-12 only, excluding version rolling range 13-28)
        let mut signaled_bits = Vec::new();
        if bip9_signaling {
            for bit in 0..13u8 {
                if (version >> bit) & 1 == 1 {
                    signaled_bits.push(bit);
                }
            }
        }

        VersionInfo {
            bits: version,
            bip9_signaling,
            version_rolling_possible,
            signaled_bits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bip9_signaling_detection() {
        // Standard BIP9 version (top 3 bits = 001)
        let info = Template::parse_version_info(0x20000000);
        assert!(info.bip9_signaling);
        assert!(!info.version_rolling_possible);
        assert!(info.signaled_bits.is_empty());

        // Non-BIP9 version (top 3 bits != 001)
        let info = Template::parse_version_info(0x00000001);
        assert!(!info.bip9_signaling);

        // Another non-BIP9 (top 3 bits = 010)
        let info = Template::parse_version_info(0x40000000);
        assert!(!info.bip9_signaling);
    }

    #[test]
    fn test_version_rolling_detection() {
        // No rolling bits set
        let info = Template::parse_version_info(0x20000000);
        assert!(!info.version_rolling_possible);

        // Bit 13 set (first rolling bit)
        let info = Template::parse_version_info(0x20002000);
        assert!(info.version_rolling_possible);

        // Bit 28 set (last rolling bit)
        let info = Template::parse_version_info(0x30000000);
        assert!(info.version_rolling_possible);

        // Multiple rolling bits set
        let info = Template::parse_version_info(0x2FFFE000);
        assert!(info.version_rolling_possible);

        // Full mask (bits 13-28 all set)
        let version_with_full_rolling = 0x20000000 | 0x1FFFE000;
        let info = Template::parse_version_info(version_with_full_rolling);
        assert!(info.version_rolling_possible);
    }

    #[test]
    fn test_signaled_bits_extraction() {
        // Bit 0 signaled
        let info = Template::parse_version_info(0x20000001);
        assert!(info.bip9_signaling);
        assert_eq!(info.signaled_bits, vec![0]);

        // Bit 1 signaled (e.g., SegWit)
        let info = Template::parse_version_info(0x20000002);
        assert_eq!(info.signaled_bits, vec![1]);

        // Multiple bits signaled (bits 0, 1, 4)
        let info = Template::parse_version_info(0x20000013);
        assert_eq!(info.signaled_bits, vec![0, 1, 4]);

        // Bit 12 signaled (last non-rolling bit)
        let info = Template::parse_version_info(0x20001000);
        assert_eq!(info.signaled_bits, vec![12]);

        // Bits in rolling range should NOT appear in signaled_bits
        // Set bit 13 (rolling range) - should not be in signaled_bits
        let info = Template::parse_version_info(0x20002000);
        assert!(info.signaled_bits.is_empty());
    }

    #[test]
    fn test_combined_signaling_and_rolling() {
        // BIP9 with both signaling bit 1 and rolling bits
        let version = 0x20000002 | 0x00004000; // bit 1 + bit 14 (rolling)
        let info = Template::parse_version_info(version);

        assert!(info.bip9_signaling);
        assert!(info.version_rolling_possible);
        assert_eq!(info.signaled_bits, vec![1]); // Only bit 1, not bit 14
    }

    #[test]
    fn test_non_bip9_has_no_signaled_bits() {
        // Even with bits set, non-BIP9 should have empty signaled_bits
        let info = Template::parse_version_info(0x00000003);
        assert!(!info.bip9_signaling);
        assert!(info.signaled_bits.is_empty());
    }

    #[test]
    fn test_real_world_versions() {
        // Typical mainnet BIP9 version
        let info = Template::parse_version_info(0x20000000);
        assert!(info.bip9_signaling);
        assert!(!info.version_rolling_possible);
        assert!(info.signaled_bits.is_empty());

        // Version with overt ASICBoost (common pattern)
        // Pools often use bits in the 13-28 range for version rolling
        let info = Template::parse_version_info(0x27FFE000);
        assert!(info.bip9_signaling);
        assert!(info.version_rolling_possible);
    }

    #[test]
    fn test_version_bits_preserved() {
        let version = 0x20004002;
        let info = Template::parse_version_info(version);
        assert_eq!(info.bits, version);
    }
}
