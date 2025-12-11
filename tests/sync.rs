use crate::test_psql::create_shares_for_user;
use {super::*, tokio_util::sync::CancellationToken};

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
        tokio::time::sleep(Duration::from_millis(200)).await;
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

    let stored_block: Option<(i32, String)> = sqlx::query_as(
        "SELECT blockheight, blockhash FROM blocks ORDER BY blockheight LIMIT 1",
    )
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

trait SyncSendTestExt {
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
