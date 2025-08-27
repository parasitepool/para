use {
    super::*,
    crate::test_psql::{
        create_test_block, create_test_shares, insert_test_block, insert_test_shares,
        setup_test_schema,
    },
    para::subcommand::sync::{ShareBatch, SyncResponse, SyncSend},
    std::sync::atomic::{AtomicUsize, Ordering},
};

static BATCH_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[tokio::test]
async fn test_sync_batch_endpoint() {
    let server = TestServer::spawn_with_sync_endpoint().await;

    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let test_shares = create_test_shares(5, 800000);
    let test_block = create_test_block(800000);

    let batch = ShareBatch {
        block: Some(test_block),
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
}

#[tokio::test]
async fn test_sync_empty_batch() {
    let server = TestServer::spawn_with_sync_endpoint().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

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
}

#[tokio::test]
async fn test_sync_batch_with_block_only() {
    let server = TestServer::spawn_with_sync_endpoint().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let test_block = create_test_block(800001);

    let batch = ShareBatch {
        block: Some(test_block),
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
}

#[tokio::test]
async fn test_sync_large_batch() {
    let record_count_in_large_batch = 93000;
    let server = TestServer::spawn_with_sync_endpoint().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let test_shares = create_test_shares(record_count_in_large_batch, 800002);
    let test_block = create_test_block(800002);

    let batch = ShareBatch {
        block: Some(test_block),
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
}

#[tokio::test]
async fn test_sync_multiple_batches_different_blocks() {
    let server = TestServer::spawn_with_sync_endpoint().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    for block_height in 800010i64..800015i64 {
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
}

#[tokio::test]
async fn test_sync_duplicate_batch_id() {
    let server = TestServer::spawn_with_sync_endpoint().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

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
}

#[tokio::test]
async fn test_sync_endpoint_to_endpoint() {
    let source_server = TestServer::spawn_with_sync_endpoint().await;
    let target_server = TestServer::spawn_with_sync_endpoint().await;

    setup_test_schema(source_server.database_url().unwrap())
        .await
        .unwrap();
    setup_test_schema(target_server.database_url().unwrap())
        .await
        .unwrap();

    insert_test_shares(source_server.database_url().unwrap(), 100, 800030)
        .await
        .unwrap();
    insert_test_block(source_server.database_url().unwrap(), 800030)
        .await
        .unwrap();

    SyncSend::default()
        .with_endpoint(target_server.url().to_string())
        .with_database_url(source_server.database_url().unwrap())
        .with_batch_size(50)
        .with_terminate_when_complete(true);

    let client = reqwest::Client::new();
    let health_check = client
        .get(target_server.url().join("/sync/batch").unwrap())
        .send()
        .await;

    assert!(health_check.is_ok());
}

trait SyncSendTestExt {
    fn with_database_url(self, database_url: String) -> Self;
    fn with_batch_size(self, batch_size: i64) -> Self;
    fn with_terminate_when_complete(self, terminate: bool) -> Self;
}

impl SyncSendTestExt for SyncSend {
    fn with_database_url(mut self, database_url: String) -> Self {
        self.database_url = database_url;
        self
    }

    fn with_batch_size(mut self, batch_size: i64) -> Self {
        self.batch_size = batch_size;
        self
    }

    fn with_terminate_when_complete(mut self, terminate: bool) -> Self {
        self.terminate_when_complete = terminate;
        self
    }
}
