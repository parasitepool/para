use {
    super::*,
    crate::stratum::{Client, ClientConfig, Event, SubscribeResult},
    bitcoin::{Transaction, TxOut, consensus::Decodable, script::ScriptBuf},
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
        help = "Show raw mining.notify data instead of interpreted output."
    )]
    pub raw: bool,
    #[arg(long, help = "Show coinbase transaction hex.")]
    pub show_coinbase_hex: bool,
    #[arg(long, help = "Show merkle branch hashes.")]
    pub show_merkle: bool,
}

/// Raw mining.notify output (used with --raw flag)
#[derive(Debug, Serialize, Deserialize)]
pub struct Output {
    pub stratum_endpoint: String,
    pub ip_address: String,
    pub timestamp: u64,
    pub extranonce1: Extranonce,
    pub extranonce2_size: usize,
    pub job_id: JobId,
    pub prevhash: PrevHash,
    pub coinb1: String,
    pub coinb2: String,
    pub merkle_branches: Vec<MerkleNode>,
    pub version: Version,
    pub nbits: Nbits,
    pub ntime: Ntime,
    pub clean_jobs: bool,
}

/// Interpreted block template output
#[derive(Debug, Serialize, Deserialize)]
pub struct InterpretedOutput {
    // Connection info
    pub stratum_endpoint: String,
    pub ip_address: String,
    pub timestamp: u64,

    // Job identification
    pub job_id: JobId,
    pub clean_jobs: bool,

    // Block header info
    pub block_header: BlockHeaderInfo,

    // Coinbase interpretation
    pub coinbase: CoinbaseInfo,

    // Mining parameters
    pub mining_params: MiningParams,

    // Optional detailed data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coinbase_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merkle_branches: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockHeaderInfo {
    pub previous_block_hash: String,
    pub version: u32,
    pub version_hex: String,
    pub version_bits: VersionBits,
    pub timestamp: u64,
    pub timestamp_human: String,
    pub bits: String,
    pub difficulty: f64,
    pub target: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VersionBits {
    /// BIP9 versionbits signaling
    pub bip9_signaling: bool,
    /// Active version bit positions (if BIP9)
    pub signaled_bits: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CoinbaseInfo {
    /// Block height extracted from coinbase (BIP34)
    pub block_height: Option<u64>,
    /// Pool signature/tag extracted from coinbase scriptSig
    pub pool_tag: Option<String>,
    /// ASCII message in coinbase (if any)
    pub coinbase_message: Option<String>,
    /// Total coinbase output value
    pub total_output_value: u64,
    pub total_output_value_btc: f64,
    /// Current block subsidy
    pub block_subsidy: u64,
    pub block_subsidy_btc: f64,
    /// Estimated fees in block (total - subsidy)
    pub estimated_fees: u64,
    pub estimated_fees_btc: f64,
    /// Coinbase outputs breakdown
    pub outputs: Vec<CoinbaseOutput>,
    /// Has SegWit witness commitment
    pub has_witness_commitment: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CoinbaseOutput {
    pub index: usize,
    pub value: u64,
    pub value_btc: f64,
    pub script_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    pub is_witness_commitment: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MiningParams {
    pub extranonce1: String,
    pub extranonce1_bytes: usize,
    pub extranonce2_size: usize,
    pub total_extranonce_size: usize,
    /// Number of transactions in block (from merkle branch count)
    pub estimated_tx_count: u64,
}

/// Data extracted from mining.notify for interpretation
struct NotifyData<'a> {
    job_id: &'a JobId,
    prevhash: &'a PrevHash,
    coinb1: &'a str,
    coinb2: &'a str,
    merkle_branches: &'a [MerkleNode],
    version: &'a Version,
    nbits: &'a Nbits,
    ntime: &'a Ntime,
    clean_jobs: bool,
}

impl Template {
    pub async fn run(self, cancel_token: CancellationToken) -> anyhow::Result<()> {
        info!(
            "Connecting to {} with user {}",
            self.stratum_endpoint, self.username
        );

        let address = resolve_stratum_endpoint(&self.stratum_endpoint).await?;

        let config = ClientConfig {
            address: address.to_string(),
            username: self.username.clone(),
            user_agent: USER_AGENT.into(),
            password: self.password.clone(),
            timeout: Duration::from_secs(5),
        };

        let client = Client::new(config);
        let mut events = client.connect().await?;

        let (subscription, _, _) = client.subscribe().await?;

        client.authorize().await?;

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Shutting down template monitor");
                    break;
                }
                event = events.recv() => {
                    match event {
                        Ok(Event::Notify(notify)) => {
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs();

                            if self.raw {
                                let output = Output {
                                    stratum_endpoint: self.stratum_endpoint.clone(),
                                    ip_address: address.ip().to_string(),
                                    timestamp,
                                    extranonce1: subscription.extranonce1.clone(),
                                    extranonce2_size: subscription.extranonce2_size,
                                    job_id: notify.job_id,
                                    prevhash: notify.prevhash,
                                    coinb1: notify.coinb1,
                                    coinb2: notify.coinb2,
                                    merkle_branches: notify.merkle_branches,
                                    version: notify.version,
                                    nbits: notify.nbits,
                                    ntime: notify.ntime,
                                    clean_jobs: notify.clean_jobs,
                                };
                                println!("{}", serde_json::to_string_pretty(&output)?);
                            } else {
                                let notify_data = NotifyData {
                                    job_id: &notify.job_id,
                                    prevhash: &notify.prevhash,
                                    coinb1: &notify.coinb1,
                                    coinb2: &notify.coinb2,
                                    merkle_branches: &notify.merkle_branches,
                                    version: &notify.version,
                                    nbits: &notify.nbits,
                                    ntime: &notify.ntime,
                                    clean_jobs: notify.clean_jobs,
                                };
                                let output = self.interpret_template(
                                    &subscription,
                                    &notify_data,
                                    &address,
                                    timestamp,
                                )?;
                                println!("{}", serde_json::to_string_pretty(&output)?);
                            }

                            if !self.watch {
                                break;
                            }
                        }
                        Ok(Event::Disconnected) => {
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

    fn interpret_template(
        &self,
        subscription: &SubscribeResult,
        notify: &NotifyData,
        address: &std::net::SocketAddr,
        timestamp: u64,
    ) -> anyhow::Result<InterpretedOutput> {
        // Build full coinbase transaction
        let extranonce2_placeholder = "00".repeat(subscription.extranonce2_size);
        let coinbase_hex = format!(
            "{}{}{}{}",
            notify.coinb1, subscription.extranonce1, extranonce2_placeholder, notify.coinb2
        );

        let coinbase_bytes = hex::decode(&coinbase_hex)?;
        let coinbase_tx = Transaction::consensus_decode(&mut Cursor::new(&coinbase_bytes))?;

        // Parse block header components (convert from hex strings)
        let version_str = notify.version.to_string();
        let version_u32 = u32::from_str_radix(&version_str, 16).unwrap_or(0);
        let ntime_str = notify.ntime.to_string();
        let ntime_u64 = u64::from_str_radix(&ntime_str, 16).unwrap_or(0);
        let nbits_str = notify.nbits.to_string();
        let nbits_u32 = u32::from_str_radix(&nbits_str, 16).unwrap_or(0);

        // Calculate difficulty and target
        let (difficulty, target_hex) = Self::calculate_difficulty_and_target(nbits_u32);

        // Parse version bits
        let version_bits = Self::parse_version_bits(version_u32);

        // Parse coinbase info
        let coinbase_info = self.parse_coinbase(&coinbase_tx, notify.coinb1)?;

        // Estimate transaction count from merkle branches
        let merkle_branch_count = notify.merkle_branches.len();
        let estimated_tx_count = if merkle_branch_count == 0 {
            1 // Just the coinbase
        } else {
            // Max transactions is 2^merkle_depth, but actual is likely less
            // This is a rough estimate
            2u64.pow(merkle_branch_count as u32)
        };

        // Format previous block hash (reverse byte order for display)
        let prevhash_display = Self::format_block_hash(&notify.prevhash.to_string());

        let block_header = BlockHeaderInfo {
            previous_block_hash: prevhash_display,
            version: version_u32,
            version_hex: format!("0x{}", version_str),
            version_bits,
            timestamp: ntime_u64,
            timestamp_human: Self::format_timestamp(ntime_u64),
            bits: nbits_str,
            difficulty,
            target: target_hex,
        };

        let extranonce1_bytes = hex::decode(subscription.extranonce1.to_string())
            .map(|b| b.len())
            .unwrap_or(0);

        let mining_params = MiningParams {
            extranonce1: subscription.extranonce1.to_string(),
            extranonce1_bytes,
            extranonce2_size: subscription.extranonce2_size,
            total_extranonce_size: extranonce1_bytes + subscription.extranonce2_size,
            estimated_tx_count,
        };

        Ok(InterpretedOutput {
            stratum_endpoint: self.stratum_endpoint.clone(),
            ip_address: address.ip().to_string(),
            timestamp,
            job_id: *notify.job_id,
            clean_jobs: notify.clean_jobs,
            block_header,
            coinbase: coinbase_info,
            mining_params,
            coinbase_hex: if self.show_coinbase_hex {
                Some(coinbase_hex)
            } else {
                None
            },
            merkle_branches: if self.show_merkle {
                Some(
                    notify
                        .merkle_branches
                        .iter()
                        .map(|m| m.to_string())
                        .collect(),
                )
            } else {
                None
            },
        })
    }

    fn parse_coinbase(&self, tx: &Transaction, coinb1: &str) -> anyhow::Result<CoinbaseInfo> {
        // Extract block height from coinbase (BIP34)
        let block_height = Self::extract_block_height(tx);

        // Extract pool tag and message from scriptSig
        let (pool_tag, coinbase_message) = Self::extract_coinbase_strings(tx, coinb1);

        // Calculate total output value
        let total_output_value: u64 = tx.output.iter().map(|o| o.value.to_sat()).sum();

        // Calculate block subsidy based on height
        let block_subsidy = block_height.map(Self::calculate_block_subsidy).unwrap_or(0);

        // Estimated fees
        let estimated_fees = total_output_value.saturating_sub(block_subsidy);

        // Check for witness commitment
        let has_witness_commitment = tx.output.iter().any(Self::is_witness_commitment);

        // Parse outputs
        let outputs: Vec<CoinbaseOutput> = tx
            .output
            .iter()
            .enumerate()
            .map(|(i, out)| {
                let is_witness_commitment = Self::is_witness_commitment(out);
                let script_type = Self::classify_script(&out.script_pubkey);
                let address = Self::extract_address(&out.script_pubkey);

                CoinbaseOutput {
                    index: i,
                    value: out.value.to_sat(),
                    value_btc: out.value.to_sat() as f64 / 100_000_000.0,
                    script_type,
                    address,
                    is_witness_commitment,
                }
            })
            .collect();

        Ok(CoinbaseInfo {
            block_height,
            pool_tag,
            coinbase_message,
            total_output_value,
            total_output_value_btc: total_output_value as f64 / 100_000_000.0,
            block_subsidy,
            block_subsidy_btc: block_subsidy as f64 / 100_000_000.0,
            estimated_fees,
            estimated_fees_btc: estimated_fees as f64 / 100_000_000.0,
            outputs,
            has_witness_commitment,
        })
    }

    fn extract_block_height(tx: &Transaction) -> Option<u64> {
        if tx.input.is_empty() {
            return None;
        }

        let script_sig = &tx.input[0].script_sig;
        let bytes = script_sig.as_bytes();

        if bytes.is_empty() {
            return None;
        }

        // BIP34: First byte is the length of the height
        let len = bytes[0] as usize;
        if len == 0 || len > 8 || bytes.len() < len + 1 {
            return None;
        }

        let mut height: u64 = 0;
        for i in 0..len {
            height |= (bytes[i + 1] as u64) << (8 * i);
        }

        Some(height)
    }

    fn extract_coinbase_strings(
        tx: &Transaction,
        _coinb1: &str,
    ) -> (Option<String>, Option<String>) {
        if tx.input.is_empty() {
            return (None, None);
        }

        let script_sig = &tx.input[0].script_sig;
        let bytes = script_sig.as_bytes();

        // Try to extract ASCII strings from the scriptSig
        let mut ascii_parts: Vec<String> = Vec::new();
        let mut current_string = String::new();

        for &byte in bytes.iter().skip(4) {
            // Skip height bytes
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

        // Common pool identifiers
        let pool_identifiers = [
            "Foundry",
            "AntPool",
            "F2Pool",
            "ViaBTC",
            "Binance",
            "Poolin",
            "BTC.com",
            "SlushPool",
            "Braiins",
            "MARA",
            "Luxor",
            "SBI",
            "SpiderPool",
            "1THash",
            "EMCD",
            "Ultimus",
            "Ocean",
            "DEMAND",
        ];

        let mut pool_tag = None;
        let mut message_parts: Vec<String> = Vec::new();

        for part in &ascii_parts {
            let is_pool = pool_identifiers
                .iter()
                .any(|id| part.to_lowercase().contains(&id.to_lowercase()));

            if is_pool && pool_tag.is_none() {
                pool_tag = Some(part.clone());
            } else {
                message_parts.push(part.clone());
            }
        }

        let coinbase_message = if message_parts.is_empty() {
            None
        } else {
            Some(message_parts.join(" "))
        };

        (pool_tag, coinbase_message)
    }

    fn calculate_block_subsidy(height: u64) -> u64 {
        let halvings = height / 210_000;
        if halvings >= 64 {
            return 0;
        }
        (50 * 100_000_000) >> halvings
    }

    fn calculate_difficulty_and_target(nbits: u32) -> (f64, String) {
        let exponent = nbits >> 24;
        let mantissa = nbits & 0x00FFFFFF;

        // Bitcoin difficulty is defined as: difficulty_1_target / current_target
        // Where difficulty_1_target has nbits = 0x1d00ffff (exponent=29, mantissa=0xFFFF)
        //
        // Rather than computing 256-bit targets that overflow u128, we use:
        // difficulty = (0xFFFF / mantissa) * 2^(8 * (29 - exponent))
        //
        // This formula derives from:
        // - diff1_target = 0xFFFF * 2^(8*(29-3)) = 0xFFFF * 2^208
        // - current_target = mantissa * 2^(8*(exponent-3))
        // - difficulty = diff1_target / current_target
        //             = (0xFFFF / mantissa) * 2^(8*(29 - exponent))
        const DIFF1_EXPONENT: i32 = 0x1d; // 29

        let difficulty = if mantissa > 0 {
            let mantissa_ratio = (0xFFFF as f64) / mantissa as f64;
            let exponent_diff = 8 * (DIFF1_EXPONENT - exponent as i32);
            mantissa_ratio * 2_f64.powi(exponent_diff)
        } else {
            0.0
        };

        // Construct the target as a hex string directly
        // target = mantissa * 2^(8*(exponent-3)) = mantissa << (8*(exponent-3))
        // This is equivalent to: mantissa as hex, padded/shifted to the right position
        let target_hex = if exponent >= 3 {
            let byte_offset = (exponent - 3) as usize;
            // Target is mantissa (3 bytes) followed by byte_offset zero bytes
            // Total length is 32 bytes (256 bits)
            let mantissa_hex = format!("{:06x}", mantissa);
            let zeros_after = "00".repeat(byte_offset);
            let total_len = 3 + byte_offset; // bytes used
            let zeros_before = "00".repeat(32_usize.saturating_sub(total_len));
            format!("{}{}{}", zeros_before, mantissa_hex, zeros_after)
        } else {
            // exponent < 3: right shift mantissa
            let shift = (3 - exponent) as usize;
            let shifted = mantissa >> (8 * shift);
            format!("{:064x}", shifted)
        };

        (difficulty, target_hex)
    }

    fn parse_version_bits(version: u32) -> VersionBits {
        // BIP9 uses bits 0-28 for signaling, with top bits set to 001
        let bip9_signaling = (version >> 29) == 0b001;

        let mut signaled_bits = Vec::new();
        if bip9_signaling {
            for bit in 0..29 {
                if (version >> bit) & 1 == 1 {
                    signaled_bits.push(bit);
                }
            }
        }

        VersionBits {
            bip9_signaling,
            signaled_bits,
        }
    }

    fn is_witness_commitment(output: &TxOut) -> bool {
        let script = output.script_pubkey.as_bytes();
        // OP_RETURN followed by witness commitment magic bytes
        script.len() >= 38
            && script[0] == 0x6a // OP_RETURN
            && script[1] == 0x24 // Push 36 bytes
            && script[2..6] == [0xaa, 0x21, 0xa9, 0xed] // Witness commitment magic
    }

    fn classify_script(script: &ScriptBuf) -> String {
        if script.is_p2pkh() {
            "P2PKH".to_string()
        } else if script.is_p2sh() {
            "P2SH".to_string()
        } else if script.is_p2wpkh() {
            "P2WPKH".to_string()
        } else if script.is_p2wsh() {
            "P2WSH".to_string()
        } else if script.is_p2tr() {
            "P2TR".to_string()
        } else if script.is_op_return() {
            if Self::is_witness_commitment_bytes(script.as_bytes()) {
                "WITNESS_COMMITMENT".to_string()
            } else {
                "OP_RETURN".to_string()
            }
        } else {
            "UNKNOWN".to_string()
        }
    }

    fn is_witness_commitment_bytes(script: &[u8]) -> bool {
        script.len() >= 38
            && script[0] == 0x6a
            && script[1] == 0x24
            && script[2..6] == [0xaa, 0x21, 0xa9, 0xed]
    }

    fn extract_address(script: &ScriptBuf) -> Option<String> {
        // Try to extract address from common script types
        // This is a simplified version - full implementation would use bitcoin::Address
        if script.is_op_return() {
            return None;
        }

        // For now, return None - full implementation would decode addresses
        // based on network and script type
        None
    }

    fn format_block_hash(prevhash: &str) -> String {
        // Stratum sends prevhash in a specific byte order
        // Reverse the byte pairs for standard block hash display
        let bytes: Vec<u8> = (0..prevhash.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(&prevhash[i..i + 2], 16).ok())
            .collect();

        // Reverse for display
        bytes.iter().rev().map(|b| format!("{:02x}", b)).collect()
    }

    fn format_timestamp(unix_time: u64) -> String {
        use std::time::{Duration, UNIX_EPOCH};
        let datetime = UNIX_EPOCH + Duration::from_secs(unix_time);
        format!("{:?}", datetime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_block_subsidy() {
        assert_eq!(Template::calculate_block_subsidy(0), 5_000_000_000);
        assert_eq!(Template::calculate_block_subsidy(209_999), 5_000_000_000);
        assert_eq!(Template::calculate_block_subsidy(210_000), 2_500_000_000);
        assert_eq!(Template::calculate_block_subsidy(420_000), 1_250_000_000);
        assert_eq!(Template::calculate_block_subsidy(630_000), 625_000_000);
        assert_eq!(Template::calculate_block_subsidy(840_000), 312_500_000);
    }

    #[test]
    fn test_parse_version_bits() {
        // Standard BIP9 version with no signals
        let bits = Template::parse_version_bits(0x20000000);
        assert!(bits.bip9_signaling);
        assert!(bits.signaled_bits.is_empty());

        // BIP9 with bit 1 signaled (e.g., SegWit)
        let bits = Template::parse_version_bits(0x20000002);
        assert!(bits.bip9_signaling);
        assert!(bits.signaled_bits.contains(&1));
    }

    #[test]
    fn test_difficulty_calculation() {
        // Mainnet genesis block nbits
        let (diff, _target) = Template::calculate_difficulty_and_target(0x1d00ffff);
        assert!((diff - 1.0).abs() < 0.001);
    }
}
