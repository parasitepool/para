use super::*;
use crate::subcommand::server::database::Database;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::Path,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::time::{Duration, sleep, timeout};
use zmq::{Context, SocketType};

const ID_FILE: &str = "current_id.txt";
const SYNC_DELAY_MS: u64 = 1000;
const BLOCKHEIGHT_CHECK_DELAY_MS: u64 = 5000;
const TARGET_ID_BUFFER: i64 = 0; // this may no longer be necessary since we are transferring by id now
const ZMQ_TIMEOUT_MS: u64 = 10000;
const MAX_RETRIES: u32 = 3;

#[derive(Debug, Parser)]
pub(crate) struct SyncSend {
    #[arg(
        long,
        help = "ZMQ endpoint to send shares to",
        default_value = "tcp://127.0.0.1:5555"
    )]
    zmq_endpoint: String,

    #[arg(
        long,
        help = "Batch size for processing shares",
        default_value = "1000000"
    )]
    batch_size: i64,

    #[arg(long, help = "Force reset current ID to 0")]
    reset_id: bool,

    #[arg(
        long,
        help = "Terminate when no more records to process",
        action = clap::ArgAction::SetTrue
    )]
    terminate_when_complete: bool,

    #[arg(
        long,
        help = "Connect to Postgres running at <DATABASE_URL>",
        default_value = "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool"
    )]
    database_url: String,
}

impl Default for SyncSend {
    fn default() -> Self {
        Self::try_parse_from(std::iter::empty::<String>()).unwrap()
    }
}

#[derive(Debug, Parser)]
pub(crate) struct SyncReceive {
    #[arg(
        long,
        help = "ZMQ endpoint to receive shares from",
        default_value = "tcp://127.0.0.1:5555"
    )]
    zmq_endpoint: String,

    #[arg(
        long,
        help = "Number of worker threads for processing received batches",
        default_value = "4"
    )]
    worker_threads: usize,

    #[arg(
        long,
        help = "Connect to Postgres running at <DATABASE_URL>",
        default_value = "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool"
    )]
    database_url: String,
}

impl Default for SyncReceive {
    fn default() -> Self {
        Self::try_parse_from(std::iter::empty::<String>()).unwrap()
    }
}

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone)]
pub(crate) struct Share {
    pub(crate) id: i64,
    pub(crate) blockheight: Option<i32>,
    pub(crate) workinfoid: Option<i64>,
    pub(crate) clientid: Option<i64>,
    pub(crate) enonce1: Option<String>,
    pub(crate) nonce2: Option<String>,
    pub(crate) nonce: Option<String>,
    pub(crate) ntime: Option<String>,
    pub(crate) diff: Option<f64>,
    pub(crate) sdiff: Option<f64>,
    pub(crate) hash: Option<String>,
    pub(crate) result: Option<bool>,
    pub(crate) reject_reason: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) errn: Option<i32>,
    pub(crate) createdate: Option<String>,
    pub(crate) createby: Option<String>,
    pub(crate) createcode: Option<String>,
    pub(crate) createinet: Option<String>,
    pub(crate) workername: Option<String>,
    pub(crate) username: Option<String>,
    pub(crate) lnurl: Option<String>,
    pub(crate) address: Option<String>,
    pub(crate) agent: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ShareBatch {
    shares: Vec<Share>,
    hostname: String,
    batch_id: u64,
    total_shares: usize,
    start_id: i64,
    end_id: i64,
}

#[derive(Serialize, Deserialize, Debug)]
struct SyncResponse {
    batch_id: u64,
    received_count: usize,
    status: String,
    error_message: Option<String>,
}

#[derive(Debug)]
enum SyncResult {
    Continue,
    Complete,
    WaitForNewBlock,
}

impl SyncSend {
    pub(crate) async fn run(self, _handle: Handle) -> Result<()> {
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag_clone = shutdown_flag.clone();

        // shutdown flags
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            println!("Received shutdown signal, stopping sync send...");
            shutdown_flag_clone.store(true, Ordering::Relaxed);
            std::process::exit(0);
        });

        println!("Starting ZMQ share sync send...");
        if !self.terminate_when_complete {
            println!("Keep-alive mode enabled - will continue running even when caught up");
        }

        let database = Database::new(self.database_url.clone()).await?;
        let context = Context::new();

        let mut current_id = self.load_current_id().await?;
        let mut caught_up_logged = false;

        if self.reset_id {
            current_id = 0;
            self.save_current_id(current_id).await?;
            println!("Reset current ID to 0");
        }

        println!("Starting sync send from ID: {current_id}");

        while !shutdown_flag.load(Ordering::Relaxed) {
            match self.sync_batch(&database, &context, &mut current_id).await {
                Ok(SyncResult::Complete) => {
                    if !self.terminate_when_complete {
                        if !caught_up_logged {
                            println!("Sync send caught up, waiting for new data...");
                            caught_up_logged = true;
                        }
                        sleep(Duration::from_millis(SYNC_DELAY_MS)).await;
                    } else {
                        println!("Sync send completed successfully");
                        break;
                    }
                }
                Ok(SyncResult::Continue) => {
                    caught_up_logged = false;
                    sleep(Duration::from_millis(SYNC_DELAY_MS)).await;
                }
                Ok(SyncResult::WaitForNewBlock) => {
                    if !caught_up_logged {
                        println!(
                            "Current and latest records have same blockheight, waiting for new block..."
                        );
                        caught_up_logged = true;
                    }
                    sleep(Duration::from_millis(BLOCKHEIGHT_CHECK_DELAY_MS)).await;
                }
                Err(e) => {
                    eprintln!("Sync send error: {e}");
                    sleep(Duration::from_millis(SYNC_DELAY_MS * 5)).await;
                }
            }
        }

        if shutdown_flag.load(Ordering::Relaxed) {
            println!("Sync send stopped due to shutdown signal");
        }

        Ok(())
    }

    async fn sync_batch(
        &self,
        database: &Database,
        context: &Context,
        current_id: &mut i64,
    ) -> Result<SyncResult> {
        let max_id = database.get_max_id().await?;

        if *current_id + TARGET_ID_BUFFER >= max_id {
            return Ok(SyncResult::Complete);
        }

        let current_blockheight = database.get_blockheight_for_id(*current_id).await?;
        let latest_blockheight = database.get_blockheight_for_id(max_id).await?;

        if let (Some(current_bh), Some(latest_bh)) = (current_blockheight, latest_blockheight) {
            if current_bh >= latest_bh {
                return Ok(SyncResult::WaitForNewBlock);
            }
        }

        let target_id = std::cmp::min(*current_id + self.batch_size, max_id - TARGET_ID_BUFFER);

        println!(
            "Fetching shares from ID {} to {} (max: {}) - blockheights: {:?} -> {:?}",
            *current_id + 1,
            target_id,
            max_id,
            current_blockheight,
            latest_blockheight
        );

        // Run share compression BEFORE transmitting
        println!(
            "Compressing shares in range {} to {}",
            *current_id + 1,
            target_id
        );
        match database
            .compress_shares_range(*current_id + 1, target_id)
            .await
        {
            Ok(compressed_count) => {
                println!("Compressed {compressed_count} share records in range");
            }
            Err(e) => {
                eprintln!(
                    "Warning: Failed to compress range {} to {}: {}",
                    *current_id + 1,
                    target_id,
                    e
                );
            }
        }

        let shares = database
            .get_shares_by_id_range(*current_id + 1, target_id)
            .await?;
        let highest_id = shares.last().map(|share| share.id);

        if shares.is_empty() {
            println!("No shares found in range, moving to next batch");
            *current_id = target_id;
            self.save_current_id(*current_id).await?;
            return Ok(SyncResult::Continue);
        }

        println!("Found {} shares to sync (after compression)", shares.len());

        // retry n times
        for attempt in 1..=MAX_RETRIES {
            match self
                .send_batch_with_timeout(context, &shares, *current_id + 1, target_id)
                .await
            {
                Ok(_) => {
                    *current_id = highest_id.unwrap_or(target_id);
                    self.save_current_id(*current_id).await?;
                    return Ok(SyncResult::Continue);
                }
                Err(e) => {
                    eprintln!("Attempt {attempt} failed: {e}");
                    if attempt == MAX_RETRIES {
                        return Err(e);
                    }
                    sleep(Duration::from_millis(1000 * attempt as u64)).await;
                }
            }
        }

        unreachable!()
    }

    async fn send_batch_with_timeout(
        &self,
        context: &Context,
        shares: &[Share],
        start_id: i64,
        end_id: i64,
    ) -> Result<()> {
        let socket = context
            .socket(SocketType::REQ)
            .map_err(|e| anyhow!("Failed to create ZMQ socket: {e}"))?;

        socket
            .connect(&self.zmq_endpoint)
            .map_err(|e| anyhow!("Failed to connect to ZMQ endpoint: {e}"))?;

        let batch_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        let batch = ShareBatch {
            shares: shares.to_vec(),
            hostname: System::host_name().ok_or(anyhow!("no hostname found"))?,
            batch_id,
            total_shares: shares.len(),
            start_id,
            end_id,
        };

        let serialized = serde_json::to_string(&batch)
            .map_err(|e| anyhow!("Failed to serialize batch: {}", e))?;

        let send_future = async {
            socket
                .send(&serialized, 0)
                .map_err(|e| anyhow!("Failed to send ZMQ message: {}", e))?;

            let response = socket
                .recv_string(0)
                .map_err(|e| anyhow!("Failed to receive ZMQ response: {}", e))?
                .map_err(|e| anyhow!("Invalid UTF-8 in ZMQ response: {:?}", e))?;

            Ok::<String, anyhow::Error>(response)
        };

        let response = timeout(Duration::from_millis(ZMQ_TIMEOUT_MS), send_future)
            .await
            .map_err(|_| anyhow!("ZMQ operation timed out after {}ms", ZMQ_TIMEOUT_MS))??;

        let sync_response: SyncResponse = serde_json::from_str(&response)
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

        println!(
            "Successfully synced batch {} with {} shares (IDs {}-{})",
            batch_id,
            shares.len(),
            start_id,
            end_id
        );

        Ok(())
    }

    async fn load_current_id(&self) -> Result<i64> {
        if Path::new(ID_FILE).exists() {
            let content = fs::read_to_string(ID_FILE)
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
        fs::write(ID_FILE, id.to_string()).map_err(|e| anyhow!("Failed to save ID file: {e}"))?;
        Ok(())
    }

    pub(crate) fn with_zmq_endpoint(mut self, zmq_endpoint: String) -> Self {
        self.zmq_endpoint = zmq_endpoint;
        self
    }
}

impl SyncReceive {
    pub(crate) async fn run(self, _handle: Handle) -> Result<()> {
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let shutdown_flag_clone = shutdown_flag.clone();

        // shutdown flags
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            println!("Received shutdown signal, stopping sync receive...");
            shutdown_flag_clone.store(true, Ordering::Relaxed);
            std::process::exit(0);
        });

        println!("Starting ZMQ share sync receive server...");
        println!("Listening on: {}", self.zmq_endpoint);
        println!("Worker threads: {}", self.worker_threads);

        let database = Database::new(self.database_url.clone()).await?;
        let context = Context::new();

        let socket = context
            .socket(SocketType::REP)
            .map_err(|e| anyhow!("Failed to create ZMQ socket: {e}"))?;

        socket
            .bind(&self.zmq_endpoint)
            .map_err(|e| anyhow!("Failed to bind to ZMQ endpoint: {e}"))?;

        socket
            .set_rcvtimeo(1000) // 1 second timeout
            .map_err(|e| anyhow!("Failed to set socket timeout: {e}"))?;

        while !shutdown_flag.load(Ordering::Relaxed) {
            match self.receive_and_process_batch(&socket, &database).await {
                Ok(true) => {
                    // success
                }
                Ok(false) => {
                    // timeout
                    continue;
                }
                Err(e) => {
                    // error
                    eprintln!("Error processing batch: {e}");
                    let error_response = SyncResponse {
                        batch_id: 0,
                        received_count: 0,
                        status: "ERROR".to_string(),
                        error_message: Some(e.to_string()),
                    };

                    if let Ok(response_json) = serde_json::to_string(&error_response) {
                        let _ = socket.send(&response_json, 0);
                    }
                }
            }
        }

        println!("Sync receive server stopped");
        Ok(())
    }

    async fn receive_and_process_batch(
        &self,
        socket: &zmq::Socket,
        database: &Database,
    ) -> Result<bool> {
        match socket.recv_string(0) {
            Ok(Ok(message)) => {
                println!("Received batch message");

                let batch: ShareBatch = serde_json::from_str(&message)
                    .map_err(|e| anyhow!("Failed to parse batch JSON: {e}"))?;

                println!(
                    "Processing batch {} with {} shares (IDs {}-{})",
                    batch.batch_id,
                    batch.shares.len(),
                    batch.start_id,
                    batch.end_id
                );

                match self.process_share_batch(&batch, database).await {
                    Ok(_) => {
                        let response = SyncResponse {
                            batch_id: batch.batch_id,
                            received_count: batch.shares.len(),
                            status: "OK".to_string(),
                            error_message: None,
                        };

                        let response_json = serde_json::to_string(&response)
                            .map_err(|e| anyhow!("Failed to serialize response: {e}"))?;

                        socket
                            .send(&response_json, 0)
                            .map_err(|e| anyhow!("Failed to send response: {e}"))?;

                        println!("Successfully processed batch {}", batch.batch_id);
                        Ok(true)
                    }
                    Err(e) => {
                        let response = SyncResponse {
                            batch_id: batch.batch_id,
                            received_count: 0,
                            status: "ERROR".to_string(),
                            error_message: Some(e.to_string()),
                        };

                        let response_json = serde_json::to_string(&response)
                            .unwrap_or_else(|_| r#"{"status":"ERROR","error_message":"Failed to serialize error response"}"#.to_string());

                        socket
                            .send(&response_json, 0)
                            .map_err(|e| anyhow!("Failed to send error response: {e}"))?;

                        Err(e)
                    }
                }
            }
            Ok(Err(e)) => Err(anyhow!("Invalid UTF-8 in received message: {:?}", e)),
            Err(zmq::Error::EAGAIN) => Ok(false),
            Err(e) => Err(anyhow!("Failed to receive ZMQ message: {e}")),
        }
    }

    async fn process_share_batch(&self, batch: &ShareBatch, database: &Database) -> Result<()> {
        println!(
            "Processing {} shares from batch {}",
            batch.shares.len(),
            batch.batch_id
        );

        if batch.shares.is_empty() {
            return Ok(());
        }

        const MAX_SHARES_PER_SUBBATCH: usize = 2500;
        let mut tx = database
            .pool
            .begin()
            .await
            .map_err(|e| anyhow!("Failed to start transaction: {e}"))?;

        // Process shares in chunks to avoid parameter limit
        for (chunk_idx, chunk) in batch.shares.chunks(MAX_SHARES_PER_SUBBATCH).enumerate() {
            println!(
                "Processing sub-batch {}/{} with {} shares",
                chunk_idx + 1,
                batch.shares.len().div_ceil(MAX_SHARES_PER_SUBBATCH),
                chunk.len()
            );

            let mut query_builder = sqlx::QueryBuilder::new(
                "INSERT INTO remote_shares (
            id, origin, blockheight, workinfoid, clientid, enonce1, nonce2, nonce, ntime,
            diff, sdiff, hash, result, reject_reason, error, errn, createdate, createby,
            createcode, createinet, workername, username, lnurl, address, agent
        ) ",
            );

            // batch our inserts to reduce number of required transactions
            query_builder.push_values(chunk, |mut b, share| {
                b.push_bind(share.id)
                    .push_bind(&batch.hostname)
                    .push_bind(share.blockheight)
                    .push_bind(share.workinfoid)
                    .push_bind(share.clientid)
                    .push_bind(&share.enonce1)
                    .push_bind(&share.nonce2)
                    .push_bind(&share.nonce)
                    .push_bind(&share.ntime)
                    .push_bind(share.diff)
                    .push_bind(share.sdiff)
                    .push_bind(&share.hash)
                    .push_bind(share.result)
                    .push_bind(&share.reject_reason)
                    .push_bind(&share.error)
                    .push_bind(share.errn)
                    .push_bind(&share.createdate)
                    .push_bind(&share.createby)
                    .push_bind(&share.createcode)
                    .push_bind(&share.createinet)
                    .push_bind(&share.workername)
                    .push_bind(&share.username)
                    .push_bind(&share.lnurl)
                    .push_bind(&share.address)
                    .push_bind(&share.agent);
            });

            query_builder.push(
                " ON CONFLICT (id, origin) DO UPDATE SET
            blockheight = EXCLUDED.blockheight,
            workinfoid = EXCLUDED.workinfoid,
            clientid = EXCLUDED.clientid,
            enonce1 = EXCLUDED.enonce1,
            nonce2 = EXCLUDED.nonce2,
            nonce = EXCLUDED.nonce,
            ntime = EXCLUDED.ntime,
            diff = EXCLUDED.diff,
            sdiff = EXCLUDED.sdiff,
            hash = EXCLUDED.hash,
            result = EXCLUDED.result,
            reject_reason = EXCLUDED.reject_reason,
            error = EXCLUDED.error,
            errn = EXCLUDED.errn,
            createdate = EXCLUDED.createdate,
            createby = EXCLUDED.createby,
            createcode = EXCLUDED.createcode,
            createinet = EXCLUDED.createinet,
            workername = EXCLUDED.workername,
            username = EXCLUDED.username,
            lnurl = EXCLUDED.lnurl,
            address = EXCLUDED.address,
            agent = EXCLUDED.agent",
            );

            let query = query_builder.build();
            query.execute(&mut *tx).await.map_err(|e| {
                anyhow!(
                    "Failed to batch insert shares in sub-batch {}: {e}",
                    chunk_idx + 1
                )
            })?;
        }

        tx.commit()
            .await
            .map_err(|e| anyhow!("Failed to commit transaction: {e}"))?;

        let total_diff: f64 = batch.shares.iter().filter_map(|s| s.diff).sum();
        let worker_count = batch
            .shares
            .iter()
            .filter_map(|s| s.workername.as_ref())
            .collect::<std::collections::HashSet<_>>()
            .len();

        let min_blockheight = batch.shares.iter().filter_map(|s| s.blockheight).min();
        let max_blockheight = batch.shares.iter().filter_map(|s| s.blockheight).max();

        println!(
            "Stored batch {} with {} shares: total difficulty: {:.2}, {} unique workers, blockheights: {:?}-{:?}, origin: {}",
            batch.batch_id,
            batch.shares.len(),
            total_diff,
            worker_count,
            min_blockheight,
            max_blockheight,
            batch.hostname // Fixed: use batch.hostname instead of self.zmq_endpoint
        );

        Ok(())
    }

    pub(crate) fn with_zmq_endpoint(mut self, zmq_endpoint: String) -> Self {
        self.zmq_endpoint = zmq_endpoint;
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

    pub(crate) async fn get_blockheight_for_id(&self, id: i64) -> Result<Option<i32>> {
        let result =
            sqlx::query_scalar::<_, Option<i32>>("SELECT blockheight FROM shares WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| anyhow!("Failed to get blockheight for ID {}: {}", id, e))?;

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
