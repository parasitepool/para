use super::*;

pub(crate) fn sync_router(config: Arc<ServerConfig>, database: Database) -> Router {
    let mut router = Router::new()
        .route(
            "/sync/batch",
            post(sync_batch).layer(DefaultBodyLimit::max(50 * MEBIBYTE)),
        )
        .layer(Extension(database));

    if let Some(token) = config.admin_token() {
        router = router.layer(bearer_auth(token))
    };

    router.layer(Extension(config))
}

/// Receive a batch of shares to sync
#[utoipa::path(
    post,
    path = "/sync/batch",
    security(("admin_token" = [])),
    request_body = ShareBatch,
    responses(
        (status = 200, description = "Batch processed", body = SyncResponse),
    ),
    tag = "sync"
)]
pub(crate) async fn sync_batch(
    Extension(database): Extension<Database>,
    Extension(config): Extension<Arc<ServerConfig>>,
    Json(batch): Json<ShareBatch>,
) -> Result<Json<SyncResponse>, StatusCode> {
    info!(
        "Received sync batch {} with {} shares from {}",
        batch.batch_id,
        batch.shares.len(),
        batch.hostname
    );

    if config.migrate_accounts() && !MIGRATION_DONE.get_or_init(|| false) {
        warn!(
            "Rejecting sync batch {} - migration in progress",
            batch.batch_id
        );
        let response = SyncResponse {
            batch_id: batch.batch_id,
            received_count: 0,
            status: "UNAVAILABLE".to_string(),
            error_message: Some("Migration in progress, try again later".to_string()),
        };
        return Ok(Json(response));
    }

    if let Some(block) = &batch.block {
        match database.upsert_block(block).await {
            Ok(was_inserted) => {
                if was_inserted {
                    info!(
                        "Successfully inserted new block at height {}",
                        block.blockheight
                    );
                } else {
                    info!(
                        "Successfully updated existing block at height {}",
                        block.blockheight
                    );
                }

                let notification_result = notifications::notify_block_found(
                    config.alerts_ntfy_channel(),
                    block.blockheight,
                    block.blockhash.clone(),
                    block.coinbasevalue.unwrap_or(0),
                    block
                        .username
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                )
                .await;

                match notification_result {
                    Ok(_) => info!("Block notification sent successfully"),
                    Err(e) => error!("Failed to send block notification: {}", e),
                }
            }
            Err(e) => error!("Warning: Failed to upsert block: {}", e),
        }
    }

    match process_share_batch(&batch, &database).await {
        Ok(_) => {
            let response = SyncResponse {
                batch_id: batch.batch_id,
                received_count: batch.shares.len(),
                status: "OK".to_string(),
                error_message: None,
            };
            info!("Successfully processed batch {}", batch.batch_id);
            Ok(Json(response))
        }
        Err(e) => {
            let response = SyncResponse {
                batch_id: batch.batch_id,
                received_count: 0,
                status: "ERROR".to_string(),
                error_message: Some(e.to_string()),
            };
            error!("Failed to process batch {}: {}", batch.batch_id, e);
            Ok(Json(response))
        }
    }
}

async fn process_share_batch(batch: &ShareBatch, database: &Database) -> Result<()> {
    info!(
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

    for (chunk_idx, chunk) in batch.shares.chunks(MAX_SHARES_PER_SUBBATCH).enumerate() {
        info!(
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

    let mut account_updates: HashMap<String, AccountUpdate> = HashMap::new();

    for share in &batch.shares {
        if let Some(username) = &share.username {
            let username = username.trim();
            if username.is_empty() {
                continue;
            }

            let entry = account_updates
                .entry(username.to_string())
                .or_insert_with(|| AccountUpdate {
                    username: username.to_string(),
                    lnurl: None,
                    total_diff: 0.0,
                    blockheights: HashSet::new(),
                });

            if share.result == Some(true) {
                if let Some(diff) = share.diff {
                    entry.total_diff += diff;
                }
                if let Some(blockheight) = share.blockheight {
                    entry.blockheights.insert(blockheight);
                }
            }

            if entry.lnurl.is_none()
                && let Some(lnurl) = &share.lnurl
            {
                let trimmed_lnurl = lnurl.trim();
                if !trimmed_lnurl.is_empty() && trimmed_lnurl.len() < 255 {
                    entry.lnurl = Some(trimmed_lnurl.to_string());
                }
            }
        }
    }

    if !account_updates.is_empty() {
        info!(
            "Updating {} accounts from batch {}",
            account_updates.len(),
            batch.batch_id
        );

        for update in account_updates.values() {
            sqlx::query(
                "
                INSERT INTO accounts (username, lnurl, total_diff, lnurl_updated_at, created_at, updated_at)
                VALUES ($1, $2, $3, NOW(), NOW(), NOW())
                ON CONFLICT (username) DO UPDATE
                SET
                    lnurl = CASE
                        WHEN accounts.lnurl IS NULL
                            AND EXCLUDED.lnurl IS NOT NULL
                        THEN EXCLUDED.lnurl
                        ELSE accounts.lnurl
                    END,
                    total_diff = accounts.total_diff + EXCLUDED.total_diff,
                    updated_at = NOW()
                ",
            )
                .bind(&update.username)
                .bind(&update.lnurl)
                .bind(update.total_diff)
                .execute(&mut *tx)
                .await
                .map_err(|e| anyhow!("Failed to update account {}: {e}", update.username))?;

            if !update.blockheights.is_empty() {
                let blockheights: Vec<i32> = update.blockheights.iter().copied().collect();
                let max_blockheight = *blockheights.iter().max().unwrap();
                sqlx::query(
                    "
                    INSERT INTO account_metadata (account_id, data, created_at, updated_at)
                    SELECT id,
                           jsonb_build_object(
                               'block_count', cardinality($2::int[]),
                               'highest_blockheight', $3
                           ),
                           NOW(), NOW()
                    FROM accounts WHERE username = $1
                    ON CONFLICT (account_id) DO UPDATE
                    SET data = account_metadata.data || jsonb_build_object(
                            'block_count',
                            COALESCE((account_metadata.data->>'block_count')::bigint, 0) +
                                (SELECT COUNT(*) FROM unnest($2::int[]) AS bh
                                 WHERE bh > COALESCE((account_metadata.data->>'highest_blockheight')::int, 0)),
                            'highest_blockheight',
                            GREATEST(COALESCE((account_metadata.data->>'highest_blockheight')::int, 0), $3)
                        ),
                        updated_at = NOW()
                    ",
                )
                    .bind(&update.username)
                    .bind(&blockheights)
                    .bind(max_blockheight)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| {
                        anyhow!(
                        "Failed to update account_metadata for {}: {e}",
                        update.username
                    )
                    })?;
            }
        }
    }

    tx.commit()
        .await
        .map_err(|e| anyhow!("Failed to commit transaction: {e}"))?;

    let total_diff: f64 = batch.shares.iter().filter_map(|s| s.diff).sum();
    let worker_count = batch
        .shares
        .iter()
        .filter_map(|s| s.workername.as_ref())
        .collect::<HashSet<_>>()
        .len();

    let min_blockheight = batch.shares.iter().filter_map(|s| s.blockheight).min();
    let max_blockheight = batch.shares.iter().filter_map(|s| s.blockheight).max();

    info!(
        "Stored batch {} with {} shares: total difficulty: {:.2}, {} unique workers, blockheights: {:?}-{:?}, origin: {}",
        batch.batch_id,
        batch.shares.len(),
        total_diff,
        worker_count,
        min_blockheight,
        max_blockheight,
        batch.hostname
    );

    Ok(())
}
