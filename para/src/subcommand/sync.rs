use super::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::time::{Duration, sleep, timeout};
use zmq::{Context, SocketType};

const HEIGHT_FILE: &str = "current_height.txt";
const SYNC_DELAY_MS: u64 = 1000;
const TARGET_BLOCK_BUFFER: i32 = 2;
const ZMQ_TIMEOUT_MS: u64 = 10000; // 10 second timeout
const MAX_RETRIES: u32 = 3;

#[derive(Debug, Parser)]
pub(crate) struct SyncSend {
    #[arg(
        long,
        help = "ZMQ endpoint to send shares to",
        default_value = "tcp://127.0.0.1:5555"
    )]
    zmq_endpoint: String,

    #[arg(long, help = "Batch size for processing shares", default_value = "1")]
    batch_size: i32,

    #[arg(long, help = "Force reset current height to 0")]
    reset_height: bool,
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
}

#[derive(sqlx::FromRow, Deserialize, Serialize, Debug, Clone)]
pub(crate) struct Share {
    pub(crate) id: i32,
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
    batch_id: u64,
    total_shares: usize,
    start_height: i32,
    end_height: i32,
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
}

impl SyncSend {
    pub(crate) async fn run(self, options: Options, _handle: Handle) -> Result<()> {
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

        let database = Database::new(&options).await?;
        let context = Context::new();

        let mut current_height = self.load_current_height().await?;

        if self.reset_height {
            current_height = 0;
            self.save_current_height(current_height).await?;
            println!("Reset current height to 0");
        }

        println!("Starting sync send from block height: {}", current_height);

        while !shutdown_flag.load(Ordering::Relaxed) {
            match self
                .sync_batch(&database, &context, &mut current_height)
                .await
            {
                Ok(SyncResult::Complete) => {
                    println!("Sync send completed successfully");
                    break;
                }
                Ok(SyncResult::Continue) => {
                    sleep(Duration::from_millis(SYNC_DELAY_MS)).await;
                }
                Err(e) => {
                    eprintln!("Sync send error: {}", e);
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
        current_height: &mut i32,
    ) -> Result<SyncResult> {
        let max_height = database.get_max_blockheight().await?;

        if *current_height + TARGET_BLOCK_BUFFER >= max_height {
            return Ok(SyncResult::Complete);
        }

        let target_height = std::cmp::min(
            *current_height + self.batch_size,
            max_height - TARGET_BLOCK_BUFFER,
        );

        println!(
            "Fetching shares from height {} to {} (max: {})",
            *current_height + 1,
            target_height,
            max_height
        );

        let shares = database
            .get_shares_by_height_range(*current_height + 1, target_height)
            .await?;

        if shares.is_empty() {
            println!("No shares found in range, moving to next batch");
            *current_height = target_height;
            self.save_current_height(*current_height).await?;
            return Ok(SyncResult::Continue);
        }

        println!("Found {} shares to sync", shares.len());

        // retry n times
        for attempt in 1..=MAX_RETRIES {
            match self
                .send_batch_with_timeout(context, &shares, *current_height + 1, target_height)
                .await
            {
                Ok(_) => {
                    *current_height = target_height;
                    self.save_current_height(*current_height).await?;
                    return Ok(SyncResult::Continue);
                }
                Err(e) => {
                    eprintln!("Attempt {} failed: {}", attempt, e);
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
        start_height: i32,
        end_height: i32,
    ) -> Result<()> {
        let socket = context
            .socket(SocketType::REQ)
            .map_err(|e| anyhow!("Failed to create ZMQ socket: {}", e))?;

        socket
            .connect(&self.zmq_endpoint)
            .map_err(|e| anyhow!("Failed to connect to ZMQ endpoint: {}", e))?;

        let batch_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();

        let batch = ShareBatch {
            shares: shares.to_vec(),
            batch_id,
            total_shares: shares.len(),
            start_height,
            end_height,
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
            "Successfully synced batch {} with {} shares (heights {}-{})",
            batch_id,
            shares.len(),
            start_height,
            end_height
        );

        Ok(())
    }

    async fn load_current_height(&self) -> Result<i32> {
        if Path::new(HEIGHT_FILE).exists() {
            let content = fs::read_to_string(HEIGHT_FILE)
                .map_err(|e| anyhow!("Failed to read height file: {}", e))?;
            let height = content
                .trim()
                .parse::<i32>()
                .map_err(|e| anyhow!("Invalid height in file: {}", e))?;
            Ok(height)
        } else {
            Ok(0)
        }
    }

    async fn save_current_height(&self, height: i32) -> Result<()> {
        fs::write(HEIGHT_FILE, height.to_string())
            .map_err(|e| anyhow!("Failed to save height file: {}", e))?;
        Ok(())
    }
}

impl SyncReceive {
    pub(crate) async fn run(self, options: Options, _handle: Handle) -> Result<()> {
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

        let database = Database::new(&options).await?;
        let context = Context::new();

        let socket = context
            .socket(SocketType::REP)
            .map_err(|e| anyhow!("Failed to create ZMQ socket: {}", e))?;

        socket
            .bind(&self.zmq_endpoint)
            .map_err(|e| anyhow!("Failed to bind to ZMQ endpoint: {}", e))?;

        socket
            .set_rcvtimeo(1000) // 1 second timeout
            .map_err(|e| anyhow!("Failed to set socket timeout: {}", e))?;

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
                    eprintln!("Error processing batch: {}", e);
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
                    .map_err(|e| anyhow!("Failed to parse batch JSON: {}", e))?;

                println!(
                    "Processing batch {} with {} shares (heights {}-{})",
                    batch.batch_id,
                    batch.shares.len(),
                    batch.start_height,
                    batch.end_height
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
                            .map_err(|e| anyhow!("Failed to serialize response: {}", e))?;

                        socket
                            .send(&response_json, 0)
                            .map_err(|e| anyhow!("Failed to send response: {}", e))?;

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
                            .map_err(|e| anyhow!("Failed to send error response: {}", e))?;

                        Err(e)
                    }
                }
            }
            Ok(Err(e)) => Err(anyhow!("Invalid UTF-8 in received message: {:?}", e)),
            Err(zmq::Error::EAGAIN) => Ok(false),
            Err(e) => Err(anyhow!("Failed to receive ZMQ message: {}", e)),
        }
    }

    // TODO: Add handling for what to do with transferred shares
    async fn process_share_batch(&self, batch: &ShareBatch, _database: &Database) -> Result<()> {
        println!(
            "Processing {} shares from batch {}",
            batch.shares.len(),
            batch.batch_id
        );

        // TODO: remove this after validating that we don't have (a)sync issues
        sleep(Duration::from_millis(2000)).await;

        if !batch.shares.is_empty() {
            let total_diff: f64 = batch.shares.iter().filter_map(|s| s.diff).sum();

            let worker_count = batch
                .shares
                .iter()
                .filter_map(|s| s.workername.as_ref())
                .collect::<std::collections::HashSet<_>>()
                .len();

            println!(
                "Batch {} summary: {} shares, total difficulty: {:.2}, {} unique workers",
                batch.batch_id,
                batch.shares.len(),
                total_diff,
                worker_count
            );
        }

        Ok(())
    }
}

impl Database {
    pub(crate) async fn get_max_blockheight(&self) -> Result<i32> {
        let result = sqlx::query_scalar::<_, Option<i32>>(
            "SELECT MAX(blockheight) FROM shares WHERE blockheight IS NOT NULL",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| anyhow!("Failed to get max block height: {}", e))?;

        Ok(result.unwrap_or(0))
    }

    pub(crate) async fn get_shares_by_height_range(
        &self,
        start_height: i32,
        end_height: i32,
    ) -> Result<Vec<Share>> {
        if start_height > end_height {
            return Err(anyhow!(
                "Invalid height range: {} > {}",
                start_height,
                end_height
            ));
        }

        sqlx::query_as::<_, Share>(
            "
            SELECT * FROM shares
            WHERE blockheight >= $1 AND blockheight <= $2
            ORDER BY blockheight ASC, id ASC
            ",
        )
        .bind(start_height)
        .bind(end_height)
        .fetch_all(&self.pool)
        .await
        .map_err(|err| anyhow!("Database query failed: {}", err))
    }
}
