use {super::*, crate::test_psql::create_shares_for_user, tokio_util::sync::CancellationToken};

pub(crate) static BATCH_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn test_sync_batch_endpoint() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_shares = create_test_shares(5, 800000);
    let test_block = create_test_block(800000);

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: test_shares,
        hostname: "test-node-1".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 5,
        start_id: 1,
        end_id: 5,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    assert_eq!(response.status, "OK");
    assert_eq!(response.received_count, 5);
    assert_eq!(response.batch_id, batch.batch_id);
    assert!(response.error_message.is_none());

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let stored_shares: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, origin FROM remote_shares WHERE origin = $1")
            .bind(&batch.hostname)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(stored_shares.len(), 5);

    let stored_block: Option<(i32, String)> =
        sqlx::query_as("SELECT blockheight, blockhash FROM blocks WHERE blockheight = $1")
            .bind(test_block.blockheight)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(stored_block.is_some());
    assert_eq!(stored_block.unwrap().1, test_block.blockhash);

    pool.close().await;
}

#[tokio::test]
async fn test_sync_with_auth() {
    let mut server = TestServer::spawn_with_db_args("--admin-token verysecrettoken").await;

    let db_url = server.database_url().unwrap();

    setup_test_schema(db_url.clone()).await.unwrap();

    let test_shares = create_test_shares(5, 800000);
    let test_block = create_test_block(800000);

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: test_shares,
        hostname: "test-node-1".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 5,
        start_id: 1,
        end_id: 5,
    };

    let fail: Response = server.post_json_raw("/sync/batch", &batch).await;

    assert_eq!(fail.status(), StatusCode::UNAUTHORIZED);

    server.admin_token = Some("verysecrettoken".into());

    let succ: SyncResponse = server.post_json("/sync/batch", &batch).await;

    assert_eq!(succ.status, "OK");
    assert_eq!(succ.received_count, 5);
    assert_eq!(succ.batch_id, batch.batch_id);
    assert!(succ.error_message.is_none());

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let stored_shares: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, origin FROM remote_shares WHERE origin = $1")
            .bind(&batch.hostname)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(stored_shares.len(), 5);

    let stored_block: Option<(i32, String)> =
        sqlx::query_as("SELECT blockheight, blockhash FROM blocks WHERE blockheight = $1")
            .bind(test_block.blockheight)
            .fetch_optional(&pool)
            .await
            .unwrap();

    assert!(stored_block.is_some());
    assert_eq!(stored_block.unwrap().1, test_block.blockhash);

    pool.close().await;
}

#[tokio::test]
async fn test_sync_empty_batch() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let batch = ShareBatch {
        block: None,
        shares: vec![],
        hostname: "test-node-empty".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    assert_eq!(response.status, "OK");
    assert_eq!(response.received_count, 0);

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let stored_shares: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, origin FROM remote_shares WHERE origin = $1")
            .bind(&batch.hostname)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(stored_shares.len(), 0);

    let stored_block: Option<(i32, String)> =
        sqlx::query_as("SELECT blockheight, blockhash FROM blocks LIMIT 1")
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(stored_block.is_none());

    pool.close().await;
}

#[tokio::test]
#[ignore]
async fn test_sync_batch_block_find_notification_e2e() {
    let channel = alerts::generate_test_channel();

    let server = TestServer::spawn_with_db_args(format!("--alerts-ntfy-channel {channel}")).await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let blockheight: i32 = 9_999_999;
    let sentinel_hash = format!("deadbeefdeadbeef{:048x}", blockheight);
    assert_eq!(sentinel_hash.len(), 64);
    let hostname = "test-block-find-e2e".to_string();

    let mut shares = create_shares_for_user("test_user", &[blockheight], 1);
    shares.extend(create_shares_for_user("other_user", &[blockheight], 2));
    assert_eq!(shares.len(), 2);

    let shares_batch = ShareBatch {
        block: None,
        shares: shares.clone(),
        hostname: hostname.clone(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: shares.len(),
        start_id: 1,
        end_id: 2,
    };
    let resp1: SyncResponse = server.post_json("/sync/batch", &shares_batch).await;
    assert_eq!(resp1.status, "OK");
    assert_eq!(resp1.received_count, 2);

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let stored_shares: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM remote_shares WHERE origin = $1")
            .bind(&hostname)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_shares, 2);

    let accounts: Vec<(String, Option<String>, i64)> =
        sqlx::query_as("SELECT username, lnurl, total_diff FROM accounts ORDER BY username")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].0, "other_user");
    assert_eq!(accounts[1].0, "test_user");
    assert!(
        accounts[0].1.is_some(),
        "other_user lnurl should be populated"
    );
    assert!(
        accounts[1].1.is_some(),
        "test_user lnurl should be populated"
    );
    assert!(accounts[0].2 > 0);
    assert!(accounts[1].2 > 0);

    // --- Batch 2: block-only batch triggering payouts + notification ---
    let test_block = FoundBlockRecord {
        id: blockheight,
        blockheight,
        blockhash: sentinel_hash.clone(),
        confirmed: Some(true),
        workername: Some("test_worker".to_string()),
        username: Some("test_user".to_string()),
        diff: Some(1_000_000.0),
        coinbasevalue: Some(625_000_000),
        rewards_processed: Some(false),
    };

    let block_batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: hostname.clone(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 3,
        end_id: 3,
    };
    let resp2: SyncResponse = server.post_json("/sync/batch", &block_batch).await;
    assert_eq!(resp2.status, "OK");

    let stored: (i32, String) =
        sqlx::query_as("SELECT blockheight, blockhash FROM blocks WHERE blockheight = $1")
            .bind(blockheight)
            .fetch_one(&pool)
            .await
            .expect("block row should exist after upsert");
    assert_eq!(stored.1, sentinel_hash);

    let payouts: Vec<(String, i64, i64, String)> = sqlx::query_as(
        "SELECT a.username, p.amount, p.diff_paid, p.status
           FROM payouts p JOIN accounts a ON a.id = p.account_id
           WHERE p.blockheight_end = $1
           ORDER BY a.username",
    )
    .bind(blockheight)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(payouts.len(), 2, "expected finder + one participant payout");

    let (other, test) = (&payouts[0], &payouts[1]);
    assert_eq!(other.0, "other_user");
    assert_eq!(other.3, "pending");
    assert!(other.1 > 0, "participant should receive a non-zero amount");

    assert_eq!(test.0, "test_user");
    assert_eq!(test.3, "success");
    assert_eq!(test.1, 0, "finder payout amount should be zero");

    tokio::time::sleep(Duration::from_millis(2500)).await;
    let received = crate::alerts::listen_for_ntfy_messages(&channel, Duration::from_secs(10)).await;
    assert!(
        received.len() >= 2,
        "expected >=2 messages (block + attachment), got {}: {received:?}",
        received.len()
    );

    let block_msg = received
        .iter()
        .find(|m| m.title.as_deref().unwrap_or("").contains("New Block Found"))
        .expect("block-found notification missing");
    let block_title = block_msg.title.clone().unwrap_or_default();
    let block_body = block_msg.message.clone().unwrap_or_default();
    assert!(
        block_title.contains("[TEST]"),
        "title missing [TEST]: {block_title:?}"
    );
    assert!(
        block_title.contains(&blockheight.to_string()),
        "title missing height: {block_title:?}"
    );
    assert!(
        block_body.contains("[TEST]"),
        "body missing [TEST]: {block_body:?}"
    );
    assert!(
        block_body.contains("6.25000000 BTC"),
        "body missing coinbase: {block_body:?}"
    );
    assert!(
        block_body.contains("test_user"),
        "body missing miner: {block_body:?}"
    );
    assert_eq!(block_msg.priority, Some(5));

    let attach_msg = received
        .iter()
        .find(|m| m.attachment.is_some())
        .expect("payouts attachment message missing");
    let attachment = attach_msg.attachment.as_ref().unwrap();
    assert_eq!(attachment.name, format!("payouts-{blockheight}.json"));
    assert!(
        attach_msg.title.as_deref().unwrap_or("").contains("[TEST]"),
        "attachment title missing [TEST]: {:?}",
        attach_msg.title
    );

    // Fetch the attachment and verify its payouts match the DB state.
    let body = reqwest::get(&attachment.url)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let payouts_json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let payouts_arr = payouts_json.as_array().expect("payouts JSON is an array");
    // Only other_user has a pending payout (finder has status='success').
    assert_eq!(
        payouts_arr.len(),
        1,
        "expected 1 pending payout, got: {body}"
    );
    let entry = &payouts_arr[0];
    assert_eq!(entry["btc_address"], "other_user");
    assert!(entry["amount_sats"].as_i64().unwrap() > 0);

    pool.close().await;
}

#[tokio::test]
async fn test_sync_batch_with_block_only() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_block = create_test_block(800001);

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node-block".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    assert_eq!(response.status, "OK");
    assert_eq!(response.received_count, 0);

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let stored_shares: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, origin FROM remote_shares WHERE origin = $1")
            .bind(&batch.hostname)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(stored_shares.len(), 0);

    let stored_block: Option<(i32, String)> =
        sqlx::query_as("SELECT blockheight, blockhash FROM blocks WHERE blockheight = $1")
            .bind(test_block.blockheight)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(stored_block.is_some());
    assert_eq!(stored_block.unwrap().1, test_block.blockhash);

    pool.close().await;
}

#[tokio::test]
#[timeout(90000)]
#[ignore]
async fn test_sync_large_batch() {
    let record_count_in_large_batch = 40000;
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_shares = create_test_shares(record_count_in_large_batch, 800002);
    let test_block = create_test_block(800002);

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: test_shares,
        hostname: "test-node-large".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: record_count_in_large_batch as usize,
        start_id: 1,
        end_id: record_count_in_large_batch as i64,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    assert_eq!(response.status, "OK");
    assert_eq!(response.received_count as u32, record_count_in_large_batch);

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let stored_shares: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, origin FROM remote_shares WHERE origin = $1")
            .bind(&batch.hostname)
            .fetch_all(&pool)
            .await
            .unwrap();

    assert_eq!(stored_shares.len() as u32, record_count_in_large_batch);

    let stored_block: Option<(i32, String)> =
        sqlx::query_as("SELECT blockheight, blockhash FROM blocks WHERE blockheight = $1")
            .bind(test_block.blockheight)
            .fetch_optional(&pool)
            .await
            .unwrap();

    assert!(stored_block.is_some());
    assert_eq!(stored_block.unwrap().1, test_block.blockhash);

    pool.close().await;
}

#[tokio::test]
async fn test_sync_multiple_batches_different_blocks() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    for block_height in 800010..800015 {
        let test_shares = create_test_shares(10, block_height);
        let test_block = create_test_block(block_height);

        let batch = ShareBatch {
            block: Some(test_block),
            shares: test_shares,
            hostname: format!("test-node-{}", block_height),
            batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
            total_shares: 10,
            start_id: (block_height - 800010) * 10 + 1,
            end_id: (block_height - 800010 + 1) * 10,
        };

        let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
        assert_eq!(response.status, "OK");
        assert_eq!(response.received_count, 10);
    }

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let stored_shares: Vec<(i64, String)> = sqlx::query_as("SELECT id, origin FROM remote_shares")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(stored_shares.len(), 50);

    for block_height in 800010..800015 {
        let test_block = create_test_block(block_height);
        let stored_block: Option<(i32, String)> =
            sqlx::query_as("SELECT blockheight, blockhash FROM blocks WHERE blockheight = $1")
                .bind(test_block.blockheight)
                .fetch_optional(&pool)
                .await
                .unwrap();

        assert!(stored_block.is_some());
        assert_eq!(stored_block.unwrap().1, test_block.blockhash);
    }

    pool.close().await;
}

#[tokio::test]
async fn test_sync_duplicate_batch_id() {
    // batch_id serves only as validation that the synced batch matches
    // test acts as a canary against changing this behavior without consideration

    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let batch_id = BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64;
    let test_shares = create_test_shares(3, 800020);

    let batch1 = ShareBatch {
        block: None,
        shares: test_shares.clone(),
        hostname: "test-node-dup1".to_string(),
        batch_id,
        total_shares: 3,
        start_id: 1,
        end_id: 3,
    };

    let batch2 = ShareBatch {
        block: None,
        shares: test_shares,
        hostname: "test-node-dup2".to_string(),
        batch_id,
        total_shares: 3,
        start_id: 4,
        end_id: 6,
    };

    let response1: SyncResponse = server.post_json("/sync/batch", &batch1).await;
    assert_eq!(response1.status, "OK");

    let response2: SyncResponse = server.post_json("/sync/batch", &batch2).await;
    assert_eq!(response2.status, "OK");

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let stored_shares: Vec<(i64, String)> = sqlx::query_as("SELECT id, origin FROM remote_shares")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(stored_shares.len() as u32, 6);

    let stored_block: Option<(i32, String)> =
        sqlx::query_as("SELECT blockheight, blockhash FROM blocks LIMIT 1")
            .fetch_optional(&pool)
            .await
            .unwrap();

    assert!(stored_block.is_none());

    pool.close().await;
}

#[tokio::test]
async fn test_sync_batch_creates_accounts() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_shares = create_test_shares(5, 800000);
    let test_block = create_test_block(800000);

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: test_shares,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 5,
        start_id: 1,
        end_id: 5,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    assert_eq!(response.status, "OK");
    assert_eq!(response.received_count, 5);

    let database = Database::new(db_url.clone()).await.unwrap();

    for i in 0..5 {
        let username = format!("user_{}", i);
        let account = database.get_account(&username).await.unwrap().unwrap();
        assert_eq!(account.btc_address, username);
        assert_eq!(
            account.ln_address,
            Some(format!("lnurl{}@test.gov", i)),
            "Account should have lnurl from share"
        );
        assert_eq!(
            account.total_diff,
            1000 + i,
            "Account should have diff from single share"
        );
    }
}

#[tokio::test]
async fn test_sync_batch_with_migrate_accounts_flag() {
    let psql_binpath = match Command::new("pg_config").arg("--bindir").output() {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .ok()
            .map(|s| PathBuf::from(s.trim())),
        _ => None,
    };
    let pg_db = PgTempDB::from_builder(PgTempDBBuilder {
        initdb_args: Default::default(),
        temp_dir_prefix: None,
        db_user: None,
        password: None,
        port: None,
        dbname: None,
        persist_data_dir: false,
        dump_path: None,
        load_path: None,
        server_configs: Default::default(),
        bin_path: psql_binpath,
    });

    let db_url = pg_db.connection_uri();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_remote_shares(db_url.clone(), 5, 800028)
        .await
        .unwrap();
    insert_test_remote_shares(db_url.clone(), 3, 800029)
        .await
        .unwrap();
    insert_test_remote_shares(db_url.clone(), 5, 800030)
        .await
        .unwrap();

    let server = TestServer::spawn_with_db_override(["--migrate-accounts"], pg_db).await;

    let database = Database::new(db_url.clone()).await.unwrap();
    let test_shares = create_test_shares(2, 800031);
    let test_block = create_test_block(800031);

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: test_shares,
        hostname: "test-node-migrate".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 2,
        start_id: 100,
        end_id: 101,
    };

    let mut response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    let mut attempts = 0;
    while response.status == "UNAVAILABLE" && attempts < 10 {
        sleep(Duration::from_millis(100)).await;
        response = server.post_json("/sync/batch", &batch).await;
        attempts += 1;
    }

    assert_eq!(response.status, "OK");
    assert_eq!(response.received_count, 2);

    let account = database.get_account("user_0").await.unwrap().unwrap();
    assert_eq!(account.btc_address, "user_0");
    assert_eq!(
        account.ln_address,
        Some("lnurl0@test.gov".to_string()),
        "Account should have lnurl from migrated share"
    );
    assert_eq!(
        account.total_diff, 4000,
        "Account in both sync and migration"
    );

    let account_new = database.get_account("user_4").await.unwrap().unwrap();
    assert_eq!(account_new.btc_address, "user_4");
    assert_eq!(
        account_new.total_diff, 2008,
        "Account not in current sync, handled by migration"
    );
}

#[tokio::test]
async fn test_sync_endpoint_to_endpoint() {
    let source_server = TestServer::spawn_with_db().await;
    let target_server = TestServer::spawn_with_db().await;

    let source_db_url = source_server.database_url().unwrap();
    setup_test_schema(source_db_url.clone()).await.unwrap();

    let target_db_url = target_server.database_url().unwrap();
    setup_test_schema(target_db_url.clone()).await.unwrap();

    for block_height in 800030..=800032 {
        insert_test_shares(source_db_url.clone(), 100, block_height)
            .await
            .unwrap();
        insert_test_block(source_db_url.clone(), block_height)
            .await
            .unwrap();
    }
    insert_test_shares(source_db_url.clone(), 1, 800033)
        .await
        .unwrap();

    let sync_sender = Sync::default()
        .with_endpoint(target_server.url().to_string())
        .with_database_url(source_db_url.clone())
        .with_terminate_when_complete(true)
        .with_temp_file();

    let client = reqwest::Client::new();

    let health_check = client
        .get(target_server.url().join("/sync/batch").unwrap())
        .send()
        .await;

    assert!(health_check.is_ok());

    sync_sender
        .run(CancellationToken::new())
        .await
        .expect("Syncing between servers failed!");

    let pool = sqlx::PgPool::connect(&target_db_url).await.unwrap();

    let stored_shares: Vec<(i64, String)> = sqlx::query_as("SELECT id, origin FROM remote_shares")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(stored_shares.len() as u32, 300);

    let stored_block: Option<(i32, String)> =
        sqlx::query_as("SELECT blockheight, blockhash FROM blocks ORDER BY blockheight LIMIT 1")
            .fetch_optional(&pool)
            .await
            .unwrap();

    assert!(stored_block.is_some());
    assert_eq!(stored_block.unwrap().0, 800030);

    let block_count: (i64, i32, i32) =
        sqlx::query_as("SELECT count(*), min(blockheight), max(blockheight) FROM blocks")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(block_count.0, 3);
    assert_eq!(block_count.1, 800030);
    assert_eq!(block_count.2, 800032);

    pool.close().await;
}

#[tokio::test]
async fn test_sync_batch_creates_block_count() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let shares = create_shares_for_user(
        "bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297",
        &[100000, 100001, 100002],
        1,
    );

    let batch = ShareBatch {
        block: None,
        shares,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 3,
        start_id: 1,
        end_id: 3,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let block_count: Option<(i64,)> = sqlx::query_as(
        "SELECT (data->>'block_count')::bigint FROM account_metadata am
         JOIN accounts a ON a.id = am.account_id
         WHERE a.username = $1",
    )
    .bind("bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297")
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert!(block_count.is_some(), "account_metadata should exist");
    assert_eq!(block_count.unwrap().0, 3, "Should count 3 distinct blocks");

    pool.close().await;
}

#[tokio::test]
async fn test_sync_batch_block_count_deduplicates_within_batch() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let shares = create_shares_for_user(
        "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
        &[200000, 200000, 200000, 200001],
        1,
    );

    let batch = ShareBatch {
        block: None,
        shares,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 4,
        start_id: 1,
        end_id: 4,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let block_count: Option<(i64,)> = sqlx::query_as(
        "SELECT (data->>'block_count')::bigint FROM account_metadata am
         JOIN accounts a ON a.id = am.account_id
         WHERE a.username = $1",
    )
    .bind("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq")
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert_eq!(
        block_count.unwrap().0,
        2,
        "Should count only 2 distinct blocks"
    );

    pool.close().await;
}

#[tokio::test]
async fn test_sync_batch_block_count_increments_across_batches() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let shares1 =
        create_shares_for_user("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy", &[300000, 300001], 1);
    let batch1 = ShareBatch {
        block: None,
        shares: shares1,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 2,
        start_id: 1,
        end_id: 2,
    };

    let response1: SyncResponse = server.post_json("/sync/batch", &batch1).await;
    assert_eq!(response1.status, "OK");

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let block_count1: (i64,) = sqlx::query_as(
        "SELECT (data->>'block_count')::bigint FROM account_metadata am
         JOIN accounts a ON a.id = am.account_id
         WHERE a.username = $1",
    )
    .bind("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(block_count1.0, 2, "Should have 2 blocks after first batch");

    let shares2 = create_shares_for_user(
        "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy",
        &[300002, 300003, 300004],
        10,
    );
    let batch2 = ShareBatch {
        block: None,
        shares: shares2,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 3,
        start_id: 10,
        end_id: 12,
    };

    let response2: SyncResponse = server.post_json("/sync/batch", &batch2).await;
    assert_eq!(response2.status, "OK");

    let block_count2: (i64,) = sqlx::query_as(
        "SELECT (data->>'block_count')::bigint FROM account_metadata am
         JOIN accounts a ON a.id = am.account_id
         WHERE a.username = $1",
    )
    .bind("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        block_count2.0, 5,
        "Should have 5 blocks total after second batch"
    );

    pool.close().await;
}

#[tokio::test]
async fn test_sync_batch_block_count_preserves_other_metadata() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let shares1 = create_shares_for_user("1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2", &[400000], 1);
    let batch1 = ShareBatch {
        block: None,
        shares: shares1,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 1,
        start_id: 1,
        end_id: 1,
    };

    let _: SyncResponse = server.post_json("/sync/batch", &batch1).await;

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    sqlx::query(
        "UPDATE account_metadata SET data = data || '{\"custom_field\": \"test_value\"}'::jsonb
         WHERE account_id = (SELECT id FROM accounts WHERE username = $1)",
    )
    .bind("1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2")
    .execute(&pool)
    .await
    .unwrap();

    let shares2 = create_shares_for_user("1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2", &[400001], 10);
    let batch2 = ShareBatch {
        block: None,
        shares: shares2,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 1,
        start_id: 10,
        end_id: 10,
    };

    let _: SyncResponse = server.post_json("/sync/batch", &batch2).await;

    let metadata: (i64, String) = sqlx::query_as(
        "SELECT (data->>'block_count')::bigint, data->>'custom_field'
         FROM account_metadata am
         JOIN accounts a ON a.id = am.account_id
         WHERE a.username = $1",
    )
    .bind("1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(metadata.0, 2, "block_count should be 2");
    assert_eq!(metadata.1, "test_value", "custom_field should be preserved");

    pool.close().await;
}

#[tokio::test]
async fn test_sync_batch_block_count_multiple_users() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let mut shares = create_shares_for_user(
        "bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297",
        &[500000, 500001],
        1,
    );
    shares.extend(create_shares_for_user(
        "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
        &[500000, 500001, 500002],
        10,
    ));

    let batch = ShareBatch {
        block: None,
        shares,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 5,
        start_id: 1,
        end_id: 12,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let first_count: (i64,) = sqlx::query_as(
        "SELECT (data->>'block_count')::bigint FROM account_metadata am
         JOIN accounts a ON a.id = am.account_id
         WHERE a.username = $1",
    )
    .bind("bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297")
    .fetch_one(&pool)
    .await
    .unwrap();

    let second_count: (i64,) = sqlx::query_as(
        "SELECT (data->>'block_count')::bigint FROM account_metadata am
         JOIN accounts a ON a.id = am.account_id
         WHERE a.username = $1",
    )
    .bind("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        first_count.0, 2,
        "bc1p5d7rjq7g6rdk2yhzks9smlaqtedr4dekq08ge8ztwac72sfr9rusxg3297 should have 2 blocks"
    );
    assert_eq!(
        second_count.0, 3,
        "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq should have 3 blocks"
    );

    pool.close().await;
}

#[tokio::test]
async fn test_sync_batch_block_count_ignores_failed_shares() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let mut shares =
        create_shares_for_user("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy", &[600000, 600001], 1);

    shares[1].result = Some(false);

    let batch = ShareBatch {
        block: None,
        shares,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 2,
        start_id: 1,
        end_id: 2,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let block_count: (i64,) = sqlx::query_as(
        "SELECT (data->>'block_count')::bigint FROM account_metadata am
         JOIN accounts a ON a.id = am.account_id
         WHERE a.username = $1",
    )
    .bind("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        block_count.0, 1,
        "Should only count block from successful share"
    );

    pool.close().await;
}

#[tokio::test]
async fn test_sync_batch_block_count_ignores_lower_blocks() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let shares1 =
        create_shares_for_user("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy", &[800005, 800006], 1);
    let batch1 = ShareBatch {
        block: None,
        shares: shares1,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 2,
        start_id: 1,
        end_id: 2,
    };

    let _: SyncResponse = server.post_json("/sync/batch", &batch1).await;

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let shares2 = create_shares_for_user(
        "3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy",
        &[800001, 800002, 800003],
        10,
    );
    let batch2 = ShareBatch {
        block: None,
        shares: shares2,
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 3,
        start_id: 10,
        end_id: 12,
    };

    let _: SyncResponse = server.post_json("/sync/batch", &batch2).await;

    let metadata: (i64, i32) = sqlx::query_as(
        "SELECT (data->>'block_count')::bigint, (data->>'highest_blockheight')::int
         FROM account_metadata am
         JOIN accounts a ON a.id = am.account_id
         WHERE a.username = $1",
    )
    .bind("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        metadata.0, 2,
        "Should still have 2 blocks - lower blocks not counted"
    );
    assert_eq!(
        metadata.1, 800006,
        "highest_blockheight unchanged at 800006"
    );

    pool.close().await;
}

pub trait SyncSendTestExt {
    fn with_database_url(self, database_url: String) -> Self;
    fn with_terminate_when_complete(self, terminate: bool) -> Self;
    fn with_temp_file(self) -> Self;
}

impl SyncSendTestExt for Sync {
    fn with_database_url(mut self, database_url: String) -> Self {
        self.database_url = database_url;
        self
    }

    fn with_terminate_when_complete(mut self, terminate: bool) -> Self {
        self.terminate_when_complete = terminate;
        self
    }

    fn with_temp_file(mut self) -> Self {
        self.id_file = tempdir()
            .unwrap()
            .path()
            .join("id.txt")
            .to_str()
            .unwrap()
            .to_string();
        self
    }
}
