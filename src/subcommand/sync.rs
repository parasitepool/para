use {
    super::*, crate::subcommand::server::database::Database, reqwest::Client, tokio::time::Duration,
};

const SYNC_DELAY_MS: u64 = 1000;
const BLOCKHEIGHT_CHECK_DELAY_MS: u64 = 5000;
const TARGET_ID_BUFFER: i64 = 0;
const HTTP_TIMEOUT_MS: u64 = 30000;
const MAX_RETRIES: u32 = 3;

#[derive(Debug, Parser)]
pub struct Sync {
    #[arg(
        long,
        help = "Send shares to HTTP <ENDPOINT>.",
        default_value = "http://127.0.0.1:8080"
    )]
    endpoint: String,
    #[arg(
        long,
        help = "Process shares in <BATCH_SIZE>.",
        default_value = "1000000"
    )]
    pub batch_size: i64,
    #[arg(long, help = "<RESET> current id to 0.")]
    reset_id: bool,
    #[arg(
        long,
        help = "Terminate when no more records to process.",
        action = clap::ArgAction::SetTrue
    )]
    pub terminate_when_complete: bool,
    #[arg(
        long,
        help = "Connect to Postgres running at <DATABASE_URL>.",
        default_value = "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool"
    )]
    pub database_url: String,
    #[arg(long, help = "<ADMIN_TOKEN> for bearer auth on sync endpoint.")]
    admin_token: Option<String>,
    #[arg(
        long,
        help = "Set <ID_FILE> to store sync progress to.",
        default_value = "current_id.txt"
    )]
    pub id_file: String,
}

impl Default for Sync {
    fn default() -> Self {
        Self::try_parse_from(std::iter::empty::<String>()).unwrap()
    }
}

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone)]
pub struct Share {
    pub id: i64,
    pub blockheight: Option<i32>,
    pub workinfoid: Option<i64>,
    pub clientid: Option<i64>,
    pub enonce1: Option<String>,
    pub nonce2: Option<String>,
    pub nonce: Option<String>,
    pub ntime: Option<String>,
    pub diff: Option<f64>,
    pub sdiff: Option<f64>,
    pub hash: Option<String>,
    pub result: Option<bool>,
    pub reject_reason: Option<String>,
    pub error: Option<String>,
    pub errn: Option<i32>,
    pub createdate: Option<String>,
    pub createby: Option<String>,
    pub createcode: Option<String>,
    pub createinet: Option<String>,
    pub workername: Option<String>,
    pub username: Option<String>,
    pub lnurl: Option<String>,
    pub address: Option<String>,
    pub agent: Option<String>,
}

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone)]
pub struct FoundBlockRecord {
    pub id: i32,
    pub blockheight: i32,
    pub blockhash: String,
    pub confirmed: Option<bool>,
    pub workername: Option<String>,
    pub username: Option<String>,
    pub diff: Option<f64>,
    pub coinbasevalue: Option<i64>,
    pub rewards_processed: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ShareBatch {
    pub block: Option<FoundBlockRecord>,
    pub shares: Vec<Share>,
    pub hostname: String,
    pub batch_id: u64,
    pub total_shares: usize,
    pub start_id: i64,
    pub end_id: i64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SyncResponse {
    pub batch_id: u64,
    pub received_count: usize,
    pub status: String,
    pub error_message: Option<String>,
}

#[derive(Debug)]
enum SyncResult {
    Continue,
    Complete,
    WaitForNewBlock,
}

impl Sync {
    pub async fn run(self) -> Result<()> {
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag_clone = shutdown_flag.clone();

        // shutdown flags
        tokio::spawn(async move {
            let _ = ctrl_c().await;
            info!("Received shutdown signal, stopping sync send...");
            shutdown_flag_clone.store(true, Ordering::Relaxed);
            process::exit(0);
        });

        info!("Starting HTTP share sync send...");
        if !self.terminate_when_complete {
            info!("Keep-alive mode enabled - will continue running even when caught up");
        }

        let database = Database::new(self.database_url.clone()).await?;
        let client = Client::new();

        let mut current_id = self.load_current_id().await?;
        let mut caught_up_logged = false;

        if self.reset_id {
            current_id = 0;
            self.save_current_id(current_id).await?;
            info!("Reset current ID to 0");
        }

        info!("Starting sync send from ID: {current_id}");

        while !shutdown_flag.load(Ordering::Relaxed) {
            match self.sync_batch(&database, &client, &mut current_id).await {
                Ok(SyncResult::Complete) => {
                    if !self.terminate_when_complete {
                        if !caught_up_logged {
                            info!("Sync send caught up, waiting for new data...");
                            caught_up_logged = true;
                        }
                        sleep(Duration::from_millis(SYNC_DELAY_MS)).await;
                    } else {
                        info!("Sync send completed successfully");
                        break;
                    }
                }
                Ok(SyncResult::Continue) => {
                    caught_up_logged = false;
                    sleep(Duration::from_millis(SYNC_DELAY_MS)).await;
                }
                Ok(SyncResult::WaitForNewBlock) => {
                    if self.terminate_when_complete {
                        info!("Sync send completed successfully");
                        break;
                    }
                    if !caught_up_logged {
                        info!(
                            "Current and latest records have same blockheight, waiting for new block..."
                        );
                        caught_up_logged = true;
                    }
                    sleep(Duration::from_millis(BLOCKHEIGHT_CHECK_DELAY_MS)).await;
                }
                Err(e) => {
                    error!("Sync send error: {e}");
                    sleep(Duration::from_millis(SYNC_DELAY_MS * 5)).await;
                }
            }
        }

        if shutdown_flag.load(Ordering::Relaxed) {
            info!("Sync send stopped due to shutdown signal");
        }

        Ok(())
    }

    async fn sync_batch(
        &self,
        database: &Database,
        client: &Client,
        current_id: &mut i64,
    ) -> Result<SyncResult> {
        let max_id = database.get_max_id().await.unwrap_or(0);

        if *current_id + TARGET_ID_BUFFER >= max_id {
            return Ok(SyncResult::Complete);
        }

        let next_id = database.get_next_id(*current_id).await.unwrap_or(0);

        let current_blockheight = database.get_blockheight_for_id(next_id).await?.unwrap_or(0);

        let last_id_in_block = database
            .get_last_id_for_blockheight(current_blockheight)
            .await;
        if last_id_in_block.is_err() {
            return Ok(SyncResult::Continue);
        }
        let target_id = std::cmp::min(*current_id + self.batch_size, last_id_in_block?.unwrap());
        let latest_blockheight = database.get_blockheight_for_id(max_id).await?;

        match (current_blockheight, latest_blockheight) {
            (current_bh, Some(latest_bh)) if current_bh >= latest_bh => {
                return Ok(SyncResult::WaitForNewBlock);
            }
            _ => {}
        }

        info!(
            "Fetching shares from ID {} to {} (max: {}) - blockheights: {:?} -> {:?}",
            next_id, target_id, max_id, current_blockheight, latest_blockheight
        );

        // Run share compression BEFORE transmitting
        info!("Compressing shares in range {} to {}", next_id, target_id);
        match database.compress_shares_range(next_id, target_id).await {
            Ok(compressed_count) => {
                info!("Compressed {compressed_count} share records in range");
            }
            Err(e) => {
                error!(
                    "Warning: Failed to compress range {} to {}: {}",
                    next_id, target_id, e
                );
            }
        }

        let shares = database.get_shares_by_id_range(next_id, target_id).await?;
        let block = database.get_block_finds(current_blockheight).await?;
        let highest_id = shares.last().map(|share| share.id).unwrap_or(target_id);

        if shares.is_empty() && block.is_none() {
            info!("No shares found in range, moving to next batch");
            *current_id = target_id;
            self.save_current_id(target_id).await?;
            return Ok(SyncResult::Continue);
        }

        info!("Found {} shares to sync (after compression)", shares.len());

        // retry n times
        for attempt in 1..=MAX_RETRIES {
            match self
                .send_batch_http(client, &block, &shares, next_id, highest_id)
                .await
            {
                Ok(_) => {
                    *current_id = highest_id;
                    self.save_current_id(*current_id).await?;
                    return Ok(SyncResult::Continue);
                }
                Err(e) => {
                    error!("Attempt {attempt} failed: {e}");
                    if attempt == MAX_RETRIES {
                        return Err(e);
                    }
                    sleep(Duration::from_millis(1000 * attempt as u64)).await;
                }
            }
        }

        unreachable!()
    }

    async fn send_batch_http(
        &self,
        client: &Client,
        block: &Option<FoundBlockRecord>,
        shares: &[Share],
        start_id: i64,
        end_id: i64,
    ) -> Result<()> {
        let batch_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        let batch = ShareBatch {
            block: block.clone(),
            shares: shares.to_vec(),
            hostname: System::host_name().ok_or(anyhow!("no hostname found"))?,
            batch_id,
            total_shares: shares.len(),
            start_id,
            end_id,
        };

        // force url format
        let url = if self.endpoint.starts_with("http") {
            format!("{}/sync/batch", self.endpoint.trim_end_matches('/'))
        } else {
            format!("http://{}/sync/batch", self.endpoint)
        };

        let mut req_client = client
            .post(&url)
            .json(&batch)
            .timeout(Duration::from_millis(HTTP_TIMEOUT_MS));

        if let Some(token) = self.get_admin_token() {
            req_client = req_client.bearer_auth(token);
        }

        let response = req_client
            .send()
            .await
            .map_err(|e| anyhow!("Failed to send HTTP request: {}", e))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "HTTP request failed with status: {}",
                response.status()
            ));
        }

        let sync_response: SyncResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse sync response: {}", e))?;

        if sync_response.status != "OK" {
            return Err(anyhow!(
                "Sync failed with status: {}. Error: {:?}",
                sync_response.status,
                sync_response.error_message
            ));
        }

        if sync_response.batch_id != batch_id {
            return Err(anyhow!(
                "Batch ID mismatch: expected {}, got {}",
                batch_id,
                sync_response.batch_id
            ));
        }

        if sync_response.received_count != shares.len() {
            return Err(anyhow!(
                "Share count mismatch: expected {}, got {}",
                shares.len(),
                sync_response.received_count
            ));
        }

        info!(
            "Successfully synced batch {} with {} shares (IDs {}-{})",
            batch_id,
            shares.len(),
            start_id,
            end_id
        );

        Ok(())
    }

    async fn load_current_id(&self) -> Result<i64> {
        if Path::new(&self.id_file).exists() {
            let content = fs::read_to_string(&self.id_file)
                .map_err(|e| anyhow!("Failed to read ID file: {}", e))?;
            let id = content
                .trim()
                .parse::<i64>()
                .map_err(|e| anyhow!("Invalid ID in file: {}", e))?;
            Ok(id)
        } else {
            Ok(0)
        }
    }

    async fn save_current_id(&self, id: i64) -> Result<()> {
        fs::write(&self.id_file, id.to_string())
            .map_err(|e| anyhow!("Failed to save ID file: {e}"))?;
        Ok(())
    }

    pub fn with_endpoint(mut self, endpoint: String) -> Self {
        self.endpoint = endpoint;
        self
    }

    fn get_admin_token(&self) -> Option<String> {
        self.admin_token.clone()
    }

    pub fn with_admin_token(mut self, admin_token: &str) -> Self {
        self.admin_token = Some(admin_token.to_string());
        self
    }
}

impl Database {
    pub(crate) async fn get_max_id(&self) -> Result<i64> {
        let result = sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(id) FROM shares")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| anyhow!("Failed to get max ID: {e}"))?;

        Ok(result.unwrap_or(0))
    }

    pub(crate) async fn get_next_id(&self, id: i64) -> Result<i64> {
        let result = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT id FROM shares WHERE id > $1 ORDER BY id ASC LIMIT 1",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| anyhow!("Failed to get next ID: {e}"))?;

        Ok(result.unwrap_or(0))
    }

    pub(crate) async fn get_blockheight_for_id(&self, id: i64) -> Result<Option<i32>> {
        let result =
            sqlx::query_scalar::<_, Option<i32>>("SELECT blockheight FROM shares WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| anyhow!("Failed to get blockheight for ID {}: {}", id, e))?;

        Ok(result.flatten())
    }

    pub(crate) async fn get_last_id_for_blockheight(
        &self,
        blockheight: i32,
    ) -> Result<Option<i64>> {
        let result = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT MAX(id) FROM shares WHERE blockheight = $1",
        )
        .bind(blockheight)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| anyhow!("Failed to get ID for blockheight {}: {}", blockheight, e))?;

        Ok(result.flatten())
    }

    pub(crate) async fn get_shares_by_id_range(
        &self,
        start_id: i64,
        end_id: i64,
    ) -> Result<Vec<Share>> {
        if start_id > end_id {
            return Err(anyhow!("Invalid ID range: {} > {}", start_id, end_id));
        }

        sqlx::query_as::<_, Share>(
            "
            SELECT * FROM shares
            WHERE id >= $1 AND id <= $2
            ORDER BY id
            ",
        )
        .bind(start_id)
        .bind(end_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!("Database query failed: {err}"))
    }

    pub(crate) async fn upsert_block(&self, block: &FoundBlockRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO blocks (
            blockheight, blockhash, confirmed, workername, username,
            diff, coinbasevalue, rewards_processed
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (blockheight) DO UPDATE SET
            blockhash = EXCLUDED.blockhash,
            confirmed = EXCLUDED.confirmed,
            workername = EXCLUDED.workername,
            username = EXCLUDED.username,
            diff = EXCLUDED.diff,
            coinbasevalue = EXCLUDED.coinbasevalue,
            rewards_processed = EXCLUDED.rewards_processed",
        )
        .bind(block.blockheight)
        .bind(&block.blockhash)
        .bind(block.confirmed)
        .bind(&block.workername)
        .bind(&block.username)
        .bind(block.diff)
        .bind(block.coinbasevalue)
        .bind(block.rewards_processed)
        .execute(&self.pool)
        .await
        .map_err(|e| anyhow!("Failed to upsert block: {e}"))?;

        Ok(())
    }

    pub(crate) async fn get_block_finds(
        &self,
        mut blockheight: i32,
    ) -> Result<Option<FoundBlockRecord>, Error> {
        if blockheight == 0 {
            blockheight = 1
        }

        sqlx::query_as::<_, FoundBlockRecord>(
            "SELECT id, blockheight, blockhash, confirmed, workername, username,
         diff, coinbasevalue, rewards_processed FROM blocks WHERE blockheight >= $1 ORDER BY blockheight ASC",
        )
        .bind(blockheight)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| anyhow!("Database query failed: {err}"))
    }

    pub(crate) async fn compress_shares_range(&self, start_id: i64, end_id: i64) -> Result<i64> {
        if start_id > end_id {
            return Err(anyhow!(
                "Invalid ID range for compression: {} > {}",
                start_id,
                end_id
            ));
        }

        let row_count =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM compress_shares($1, $2)")
                .bind(start_id)
                .bind(end_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|err| anyhow!("Compression failed: {err}"))?;

        Ok(row_count)
    }
}
