use super::*;

static BATCH_COUNTER: AtomicUsize = AtomicUsize::new(0);

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
        let account = database.get_account(&username).await.unwrap();
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
    let server = TestServer::spawn_with_db_args("--migrate-accounts").await;
    let db_url = server.database_url().unwrap();
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

    let database = Database::new(db_url.clone()).await.unwrap();

    let account_before = database.get_account("user_0").await;
    assert!(
        account_before.is_err(),
        "Verify trigger is not creating account record"
    );

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

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    assert_eq!(response.status, "OK");
    assert_eq!(response.received_count, 2);

    let account = database.get_account("user_0").await.unwrap();
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

    let account_new = database.get_account("user_4").await.unwrap();
    assert_eq!(account_new.btc_address, "user_4");
    assert_eq!(
        account_new.total_diff, 2008,
        "Account not in current sync, handled by migration"
    );
}
