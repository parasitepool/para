use super::*;

#[tokio::test]
async fn test_block_insertion_creates_payouts() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_remote_shares(db_url.clone(), 10, 800000)
        .await
        .unwrap();

    let database = Database::new(db_url.clone()).await.unwrap();
    database.migrate_accounts().await.unwrap();

    let mut test_block = create_test_block(800000);
    test_block.coinbasevalue = Some(625000000);
    test_block.username = Some("user_0".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let payout_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payouts")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(payout_count > 0, "Should create payouts for accounts");

    let finder_payout: Option<(i64, String)> = sqlx::query_as(
        "SELECT p.amount, p.status
         FROM payouts p
         JOIN accounts a ON p.account_id = a.id
         WHERE a.username = $1",
    )
    .bind("user_0")
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert!(finder_payout.is_some());
    let (amount, status) = finder_payout.unwrap();
    assert_eq!(amount, 0, "Block finder should have zero payout amount");
    assert_eq!(
        status, "success",
        "Block finder payout should be marked success"
    );

    let other_payouts: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT a.username, p.amount, p.status
         FROM payouts p
         JOIN accounts a ON p.account_id = a.id
         WHERE a.username != $1
         ORDER BY a.username",
    )
    .bind("user_0")
    .fetch_all(&pool)
    .await
    .unwrap();

    assert!(!other_payouts.is_empty(), "Other users should have payouts");
    for (username, amount, status) in &other_payouts {
        assert!(
            *amount > 0,
            "User {} should have positive payout amount",
            username
        );
        assert_eq!(
            status, "pending",
            "User {} payout should be pending",
            username
        );
    }

    pool.close().await;
}

#[tokio::test]
async fn test_block_update_does_not_create_duplicate_payouts() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_remote_shares(db_url.clone(), 5, 800001)
        .await
        .unwrap();

    let database = Database::new(db_url.clone()).await.unwrap();
    database.migrate_accounts().await.unwrap();

    let mut test_block = create_test_block(800001);
    test_block.coinbasevalue = Some(625000000);
    test_block.username = Some("user_1".to_string());

    let batch1 = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node-1".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response1: SyncResponse = server.post_json("/sync/batch", &batch1).await;
    assert_eq!(response1.status, "OK");

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let initial_payout_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payouts")
        .fetch_one(&pool)
        .await
        .unwrap();

    test_block.confirmed = Some(true);

    let batch2 = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node-2".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response2: SyncResponse = server.post_json("/sync/batch", &batch2).await;
    assert_eq!(response2.status, "OK");

    let final_payout_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payouts")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(
        initial_payout_count, final_payout_count,
        "Block update should not create duplicate payouts"
    );

    pool.close().await;
}

#[tokio::test]
async fn test_payout_distribution_proportional_to_diff() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(
        db_url.clone(),
        "user_a",
        Some("user_a@ln.com"),
        vec![],
        1000,
    )
    .await
    .unwrap();
    insert_test_account(
        db_url.clone(),
        "user_b",
        Some("user_b@ln.com"),
        vec![],
        2000,
    )
    .await
    .unwrap();
    insert_test_account(
        db_url.clone(),
        "user_c",
        Some("user_c@ln.com"),
        vec![],
        3000,
    )
    .await
    .unwrap();

    let mut test_block = create_test_block(800002);
    test_block.coinbasevalue = Some(600000000);
    test_block.username = Some("user_d".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let payouts: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT a.username, p.amount, p.diff_paid
         FROM payouts p
         JOIN accounts a ON p.account_id = a.id
         WHERE p.status = 'pending'
         ORDER BY a.username",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(payouts.len(), 3);

    let (username_a, amount_a, diff_a) = &payouts[0];
    assert_eq!(username_a, "user_a");
    assert_eq!(*diff_a, 1000);
    assert_eq!(*amount_a, 100000000);

    let (username_b, amount_b, diff_b) = &payouts[1];
    assert_eq!(username_b, "user_b");
    assert_eq!(*diff_b, 2000);
    assert_eq!(*amount_b, 199999999, "FLOOR should round down");

    let (username_c, amount_c, diff_c) = &payouts[2];
    assert_eq!(username_c, "user_c");
    assert_eq!(*diff_c, 3000);
    assert_eq!(*amount_c, 300000000);

    pool.close().await;
}

#[tokio::test]
async fn test_payout_excludes_cancelled_payouts_from_calculation() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(
        db_url.clone(),
        "user_x",
        Some("user_x@ln.com"),
        vec![],
        2000,
    )
    .await
    .unwrap();

    let account_id: i64 = sqlx::query_scalar("SELECT id FROM accounts WHERE username = 'user_x'")
        .fetch_one(&pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO payouts (account_id, amount, diff_paid, blockheight_start, blockheight_end, status)
         VALUES ($1, 50000000, 500, 0, 800002, 'cancelled')",
    )
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();

    let mut test_block = create_test_block(800003);
    test_block.coinbasevalue = Some(300000000);
    test_block.username = Some("user_y".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let payout: Option<(i64, i64)> = sqlx::query_as(
        "SELECT amount, diff_paid FROM payouts
         WHERE account_id = $1 AND status = 'pending'",
    )
    .bind(account_id)
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert!(payout.is_some());
    let (amount, diff_paid) = payout.unwrap();
    assert_eq!(diff_paid, 2000, "Should pay for full unpaid diff");
    assert_eq!(amount, 300000000, "Should receive full reward");

    pool.close().await;
}

#[tokio::test]
async fn test_payout_considers_previous_successful_payouts() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(
        db_url.clone(),
        "user_z",
        Some("user_z@ln.com"),
        vec![],
        3000,
    )
    .await
    .unwrap();

    let account_id: i64 = sqlx::query_scalar("SELECT id FROM accounts WHERE username = 'user_z'")
        .fetch_one(&pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO payouts (account_id, amount, diff_paid, blockheight_start, blockheight_end, status)
         VALUES ($1, 100000000, 1000, 0, 800004, 'success')",
    )
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();

    let mut test_block = create_test_block(800005);
    test_block.coinbasevalue = Some(400000000);
    test_block.username = Some("user_w".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let new_payout: Option<(i64, i64)> = sqlx::query_as(
        "SELECT amount, diff_paid FROM payouts
         WHERE account_id = $1 AND blockheight_end = 800005",
    )
    .bind(account_id)
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert!(new_payout.is_some());
    let (amount, diff_paid) = new_payout.unwrap();
    assert_eq!(
        diff_paid, 2000,
        "Should pay for only unpaid diff (3000 - 1000)"
    );
    assert_eq!(amount, 400000000, "Should receive full reward");

    pool.close().await;
}

#[tokio::test]
async fn test_no_payouts_when_all_diff_already_paid() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(
        db_url.clone(),
        "user_paid",
        Some("paid@ln.com"),
        vec![],
        1000,
    )
    .await
    .unwrap();

    let account_id: i64 =
        sqlx::query_scalar("SELECT id FROM accounts WHERE username = 'user_paid'")
            .fetch_one(&pool)
            .await
            .unwrap();

    sqlx::query(
        "INSERT INTO payouts (account_id, amount, diff_paid, blockheight_start, blockheight_end, status)
         VALUES ($1, 100000000, 1000, 0, 800006, 'success')",
    )
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();

    let mut test_block = create_test_block(800007);
    test_block.coinbasevalue = Some(500000000);
    test_block.username = Some("user_v".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let new_payout_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM payouts
         WHERE account_id = $1 AND blockheight_end = 800007",
    )
    .bind(account_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        new_payout_count, 0,
        "Should not create payout when all diff already paid"
    );

    pool.close().await;
}

#[tokio::test]
async fn test_payout_blockheight_range_correct() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_remote_shares(db_url.clone(), 5, 800008)
        .await
        .unwrap();

    let database = Database::new(db_url.clone()).await.unwrap();
    database.migrate_accounts().await.unwrap();

    let mut test_block_1 = create_test_block(800008);
    test_block_1.coinbasevalue = Some(600000000);
    test_block_1.username = Some("user_u".to_string());

    let batch1 = ShareBatch {
        block: Some(test_block_1.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let _response1: SyncResponse = server.post_json("/sync/batch", &batch1).await;

    let payout_1: (i32, i32) = sqlx::query_as(
        "SELECT blockheight_start, blockheight_end FROM payouts
         WHERE blockheight_end = 800008 LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        payout_1.0, 0,
        "First block should have blockheight_start = 0 (no previous block)"
    );
    assert_eq!(payout_1.1, 800008, "Should have correct blockheight_end");

    insert_test_remote_shares(db_url.clone(), 5, 800009)
        .await
        .unwrap();
    database.migrate_accounts().await.unwrap();

    let mut test_block_2 = create_test_block(800009);
    test_block_2.coinbasevalue = Some(600000000);
    test_block_2.username = Some("user_t".to_string());

    let batch2 = ShareBatch {
        block: Some(test_block_2.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let _response2: SyncResponse = server.post_json("/sync/batch", &batch2).await;

    let payout_2: (i32, i32) = sqlx::query_as(
        "SELECT blockheight_start, blockheight_end FROM payouts
         WHERE blockheight_end = 800009 LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        payout_2.0, 800008,
        "Second block should have blockheight_start = previous block height"
    );
    assert_eq!(payout_2.1, 800009, "Should have correct blockheight_end");

    pool.close().await;
}

#[tokio::test]
async fn test_block_without_coinbasevalue_no_payouts() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_remote_shares(db_url.clone(), 5, 800010)
        .await
        .unwrap();

    let database = Database::new(db_url.clone()).await.unwrap();
    database.migrate_accounts().await.unwrap();

    let mut test_block = create_test_block(800010);
    test_block.coinbasevalue = None;

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    let payout_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payouts")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(
        payout_count, 0,
        "Should not create payouts when block has no coinbasevalue"
    );

    pool.close().await;
}

#[tokio::test]
async fn test_multiple_users_with_finder_exclusion() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(db_url.clone(), "miner_1", Some("m1@ln.com"), vec![], 1000)
        .await
        .unwrap();
    insert_test_account(db_url.clone(), "miner_2", Some("m2@ln.com"), vec![], 2000)
        .await
        .unwrap();
    insert_test_account(db_url.clone(), "miner_3", Some("m3@ln.com"), vec![], 3000)
        .await
        .unwrap();
    insert_test_account(
        db_url.clone(),
        "finder",
        Some("finder@ln.com"),
        vec![],
        4000,
    )
    .await
    .unwrap();

    let mut test_block = create_test_block(800011);
    test_block.coinbasevalue = Some(1000000000);
    test_block.username = Some("finder".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let response: SyncResponse = server.post_json("/sync/batch", &batch).await;
    assert_eq!(response.status, "OK");

    let finder_payout: (i64, String, i64) = sqlx::query_as(
        "SELECT p.amount, p.status, p.diff_paid
         FROM payouts p
         JOIN accounts a ON p.account_id = a.id
         WHERE a.username = 'finder'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(finder_payout.0, 0, "Finder should have zero amount");
    assert_eq!(
        finder_payout.1, "success",
        "Finder should have success status"
    );
    assert_eq!(
        finder_payout.2, 4000,
        "Finder's diff should be marked as paid"
    );

    let other_payouts: Vec<(String, i64, String, i64)> = sqlx::query_as(
        "SELECT a.username, p.amount, p.status, p.diff_paid
         FROM payouts p
         JOIN accounts a ON p.account_id = a.id
         WHERE a.username != 'finder'
         ORDER BY a.username",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(other_payouts.len(), 3);

    assert_eq!(other_payouts[0].0, "miner_1");
    assert_eq!(other_payouts[0].1, 166666666);
    assert_eq!(other_payouts[0].2, "pending");
    assert_eq!(other_payouts[0].3, 1000);

    assert_eq!(other_payouts[1].0, "miner_2");
    assert_eq!(other_payouts[1].1, 333333333);
    assert_eq!(other_payouts[1].2, "pending");
    assert_eq!(other_payouts[1].3, 2000);

    assert_eq!(other_payouts[2].0, "miner_3");
    assert_eq!(other_payouts[2].1, 500000000);
    assert_eq!(other_payouts[2].2, "pending");
    assert_eq!(other_payouts[2].3, 3000);

    pool.close().await;
}

#[tokio::test]
async fn test_get_pending_payouts_groups_by_address() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(
        db_url.clone(),
        "user_a",
        Some("shared@ln.com"),
        vec![],
        5000,
    )
    .await
    .unwrap();
    insert_test_account(
        db_url.clone(),
        "user_b",
        Some("shared@ln.com"),
        vec![],
        3000,
    )
    .await
    .unwrap();
    insert_test_account(
        db_url.clone(),
        "user_c",
        Some("unique@ln.com"),
        vec![],
        2000,
    )
    .await
    .unwrap();

    let mut test_block = create_test_block(800012);
    test_block.coinbasevalue = Some(1000000000);
    test_block.username = Some("finder".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let _response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    use para::subcommand::server::database::PendingPayout;
    let pending: Vec<PendingPayout> = server.get_json_async("/payouts/800012").await;

    assert_eq!(pending.len(), 2, "Should have 2 grouped payouts");

    let shared_payout = pending
        .iter()
        .find(|p| p.ln_address == "shared@ln.com")
        .expect("Should have payout for shared@ln.com");

    assert_eq!(
        shared_payout.payout_ids.len(),
        2,
        "Should combine 2 payouts for shared address"
    );
    assert!(shared_payout.amount_sats > 0, "Should have combined amount");

    let unique_payout = pending
        .iter()
        .find(|p| p.ln_address == "unique@ln.com")
        .expect("Should have payout for unique@ln.com");

    assert_eq!(
        unique_payout.payout_ids.len(),
        1,
        "Should have 1 payout for unique address"
    );
    assert!(unique_payout.amount_sats > 0, "Should have amount");

    pool.close().await;
}

#[tokio::test]
async fn test_get_pending_payouts_excludes_success_status() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(db_url.clone(), "user_1", Some("user1@ln.com"), vec![], 3000)
        .await
        .unwrap();
    insert_test_account(db_url.clone(), "user_2", Some("user2@ln.com"), vec![], 2000)
        .await
        .unwrap();

    let mut test_block = create_test_block(800013);
    test_block.coinbasevalue = Some(500000000);
    test_block.username = Some("finder".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let _response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    let payout_id: i64 = sqlx::query_scalar(
        "SELECT p.id FROM payouts p
         JOIN accounts a ON p.account_id = a.id
         WHERE a.username = 'user_1' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    sqlx::query("UPDATE payouts SET status = 'success' WHERE id = $1")
        .bind(payout_id)
        .execute(&pool)
        .await
        .unwrap();

    use para::subcommand::server::database::PendingPayout;
    let pending: Vec<PendingPayout> = server.get_json_async("/payouts/800013").await;

    assert_eq!(
        pending.len(),
        1,
        "Should only return pending/failure payouts"
    );
    assert_eq!(
        pending[0].ln_address, "user2@ln.com",
        "Should only have user_2's payout"
    );

    pool.close().await;
}

#[tokio::test]
async fn test_get_pending_payouts_includes_failure_status() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(
        db_url.clone(),
        "retry_user",
        Some("retry@ln.com"),
        vec![],
        4000,
    )
    .await
    .unwrap();

    let mut test_block = create_test_block(800014);
    test_block.coinbasevalue = Some(400000000);
    test_block.username = Some("finder".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let _response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    sqlx::query(
        "UPDATE payouts SET status = 'failure', failure_reason = 'Network timeout'
         WHERE blockheight_end = 800014",
    )
    .execute(&pool)
    .await
    .unwrap();

    use para::subcommand::server::database::PendingPayout;
    let pending: Vec<PendingPayout> = server.get_json_async("/payouts/800014").await;

    assert_eq!(pending.len(), 1, "Should include failed payouts for retry");
    assert_eq!(pending[0].ln_address, "retry@ln.com");

    pool.close().await;
}

#[tokio::test]
async fn test_get_pending_payouts_excludes_no_ln_address() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(db_url.clone(), "has_ln", Some("hasln@ln.com"), vec![], 3000)
        .await
        .unwrap();
    insert_test_account(db_url.clone(), "no_ln", None, vec![], 2000)
        .await
        .unwrap();

    let mut test_block = create_test_block(800015);
    test_block.coinbasevalue = Some(500000000);
    test_block.username = Some("finder".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let _response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    use para::subcommand::server::database::PendingPayout;
    let pending: Vec<PendingPayout> = server.get_json_async("/payouts/800015").await;

    assert_eq!(
        pending.len(),
        1,
        "Should exclude accounts without LN address"
    );
    assert_eq!(pending[0].ln_address, "hasln@ln.com");

    pool.close().await;
}

#[tokio::test]
async fn test_update_payout_status_to_success() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(
        db_url.clone(),
        "payout_user",
        Some("pay@ln.com"),
        vec![],
        5000,
    )
    .await
    .unwrap();

    let mut test_block = create_test_block(800016);
    test_block.coinbasevalue = Some(600000000);
    test_block.username = Some("finder".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let _response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    let payout_id: i64 = sqlx::query_scalar(
        "SELECT id FROM payouts WHERE blockheight_end = 800016 AND status = 'pending' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    use para::subcommand::server::database::UpdatePayoutStatusRequest;
    let update_request = UpdatePayoutStatusRequest {
        payout_ids: vec![payout_id],
        status: "success".to_string(),
        failure_reason: None,
    };

    let response: serde_json::Value = server.post_json("/payouts/update", &update_request).await;

    assert_eq!(response["status"], "OK");
    assert_eq!(response["rows_affected"], 1);

    let status: String = sqlx::query_scalar("SELECT status FROM payouts WHERE id = $1")
        .bind(payout_id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, "success");

    pool.close().await;
}

#[tokio::test]
async fn test_update_payout_status_to_failure_with_reason() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(
        db_url.clone(),
        "fail_user",
        Some("fail@ln.com"),
        vec![],
        3000,
    )
    .await
    .unwrap();

    let mut test_block = create_test_block(800017);
    test_block.coinbasevalue = Some(400000000);
    test_block.username = Some("finder".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let _response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    let payout_id: i64 = sqlx::query_scalar(
        "SELECT id FROM payouts WHERE blockheight_end = 800017 AND status = 'pending' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    use para::subcommand::server::database::UpdatePayoutStatusRequest;
    let update_request = UpdatePayoutStatusRequest {
        payout_ids: vec![payout_id],
        status: "failure".to_string(),
        failure_reason: Some("Lightning network unreachable".to_string()),
    };

    let response: serde_json::Value = server.post_json("/payouts/update", &update_request).await;

    assert_eq!(response["status"], "OK");
    assert_eq!(response["rows_affected"], 1);

    let (status, failure_reason): (String, Option<String>) =
        sqlx::query_as("SELECT status, failure_reason FROM payouts WHERE id = $1")
            .bind(payout_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, "failure");
    assert_eq!(
        failure_reason,
        Some("Lightning network unreachable".to_string())
    );

    pool.close().await;
}

#[tokio::test]
async fn test_update_multiple_payout_status() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    insert_test_account(
        db_url.clone(),
        "multi_1",
        Some("multi1@ln.com"),
        vec![],
        2000,
    )
    .await
    .unwrap();
    insert_test_account(
        db_url.clone(),
        "multi_2",
        Some("multi2@ln.com"),
        vec![],
        3000,
    )
    .await
    .unwrap();
    insert_test_account(
        db_url.clone(),
        "multi_3",
        Some("multi3@ln.com"),
        vec![],
        1000,
    )
    .await
    .unwrap();

    let mut test_block = create_test_block(800018);
    test_block.coinbasevalue = Some(600000000);
    test_block.username = Some("finder".to_string());

    let batch = ShareBatch {
        block: Some(test_block.clone()),
        shares: vec![],
        hostname: "test-node".to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 0,
        start_id: 1,
        end_id: 1,
    };

    let _response: SyncResponse = server.post_json("/sync/batch", &batch).await;

    let payout_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM payouts WHERE blockheight_end = 800018 AND status = 'pending'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert!(payout_ids.len() >= 3, "Should have at least 3 payouts");

    use para::subcommand::server::database::UpdatePayoutStatusRequest;
    let update_request = UpdatePayoutStatusRequest {
        payout_ids: payout_ids.clone(),
        status: "processing".to_string(),
        failure_reason: None,
    };

    let response: serde_json::Value = server.post_json("/payouts/update", &update_request).await;

    assert_eq!(response["status"], "OK");
    assert_eq!(
        response["rows_affected"],
        payout_ids.len() as u64,
        "Should update all payouts"
    );

    let processing_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM payouts WHERE blockheight_end = 800018 AND status = 'processing'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(processing_count, payout_ids.len() as i64);

    pool.close().await;
}

#[tokio::test]
async fn test_update_payout_status_empty_list() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    use para::subcommand::server::database::UpdatePayoutStatusRequest;
    let update_request = UpdatePayoutStatusRequest {
        payout_ids: vec![],
        status: "success".to_string(),
        failure_reason: None,
    };

    let response: serde_json::Value = server.post_json("/payouts/update", &update_request).await;

    assert_eq!(response["status"], "OK");
    assert_eq!(response["rows_affected"], 0);
}

#[tokio::test]
async fn test_get_pending_payouts_empty_block() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    use para::subcommand::server::database::PendingPayout;
    let pending: Vec<PendingPayout> = server.get_json_async("/payouts/999999").await;

    assert_eq!(
        pending.len(),
        0,
        "Should return empty list for non-existent block"
    );
}
