use {
    super::*,
    crate::test_psql::{
        create_test_block, create_test_shares, insert_test_block, insert_test_shares,
        setup_test_schema,
    },
    crate::test_server::Credentials,
    tempfile::tempdir,
};

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
    let mut server =
        TestServer::spawn_with_db_args("--username test_user --password test_pass").await;
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

    server.credentials = Some(Credentials {
        username: "test_user".into(),
        password: "test_pass".into(),
    });
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

    let sync_sender = SyncSend::default()
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
        .run()
        .await
        .expect("Syncing between servers failed!");

    let pool = sqlx::PgPool::connect(&target_db_url).await.unwrap();

    let stored_shares: Vec<(i64, String)> = sqlx::query_as("SELECT id, origin FROM remote_shares")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(stored_shares.len() as u32, 300);

    let stored_block: Option<(i32, String)> =
        sqlx::query_as("SELECT blockheight, blockhash FROM blocks ORDER BY blockheight ASC LIMIT 1")
            .fetch_optional(&pool)
            .await
            .unwrap();

    assert!(stored_block.is_some());
    assert_eq!(stored_block.unwrap().0, 800030);

    let block_count: Vec<(i64, i32, i32)> =
        sqlx::query_as("SELECT count(*), min(blockheight), max(blockheight) FROM blocks")
            .fetch_all(&pool)
            .await
            .unwrap();

    assert_eq!(block_count[0].0, 3);
    assert_eq!(block_count[0].1, 800030);
    assert_eq!(block_count[0].2, 800032);

    pool.close().await;
}

trait SyncSendTestExt {
    fn with_database_url(self, database_url: String) -> Self;
    fn with_terminate_when_complete(self, terminate: bool) -> Self;
    fn with_temp_file(self) -> Self;
}

impl SyncSendTestExt for SyncSend {
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
