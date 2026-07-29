use super::*;

#[derive(Deserialize, Debug)]
struct Round {
    blockheight: i32,
    blockhash: String,
}

#[derive(Deserialize, Debug)]
struct RoundParticipant {
    username: String,
    blocks_participated: i64,
    top_diff: f64,
    total_work: f64,
}

async fn insert_test_shares_for_round(
    database_url: String,
    users: Vec<(&str, f64)>,
    blockheight: i64,
    id_offset: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::PgPool::connect(&database_url).await?;

    for (i, (username, sdiff)) in users.iter().enumerate() {
        let share_id = id_offset + i as i64;

        sqlx::query(
            "INSERT INTO remote_shares (
                id, origin, blockheight, workinfoid, clientid, enonce1, nonce2,
                nonce, ntime, diff, sdiff, hash, result, workername, username,
                createdate, createby, createcode, createinet
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19
            )",
        )
        .bind(share_id)
        .bind("test_origin")
        .bind(blockheight as i32)
        .bind(1i64)
        .bind(1i64)
        .bind("enonce1")
        .bind("nonce2")
        .bind("nonce")
        .bind("ntime")
        .bind(sdiff)
        .bind(sdiff)
        .bind("hash")
        .bind(true)
        .bind(format!("{}_worker", username))
        .bind(*username)
        .bind("2024-01-01 12:00:00")
        .bind("test")
        .bind("test")
        .bind("127.0.0.1")
        .execute(&pool)
        .await?;
    }

    pool.close().await;
    Ok(())
}

async fn insert_test_shares_with_diff(
    database_url: String,
    shares: Vec<(String, f64)>,
    block_height: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    use sqlx::{Pool, Postgres, postgres::PgPoolOptions};

    let pool: Pool<Postgres> = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;

    for (i, (username, diff)) in shares.iter().enumerate() {
        let share_id = block_height * 10000 + i as i64;

        sqlx::query(
            "INSERT INTO remote_shares (
                id, origin, blockheight, workinfoid, clientid, enonce1, nonce2,
                nonce, ntime, diff, sdiff, hash, result, workername, username,
                createdate, createby, createcode, createinet
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19
            )",
        )
        .bind(share_id)
        .bind("test_origin")
        .bind(block_height as i32)
        .bind(1i64)
        .bind(1i64)
        .bind("enonce1")
        .bind("nonce2")
        .bind("nonce")
        .bind("ntime")
        .bind(1)
        .bind(diff)
        .bind("hash")
        .bind(true)
        .bind(format!("{}_worker", username))
        .bind(username)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind("test")
        .bind("test")
        .bind("127.0.0.1")
        .execute(&pool)
        .await?;
    }

    pool.close().await;
    Ok(())
}

async fn insert_test_shares_remote(
    database_url: String,
    count: u32,
    block_height: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    use {
        crate::address,
        sqlx::{Pool, Postgres, postgres::PgPoolOptions},
    };

    let pool: Pool<Postgres> = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;

    for i in 0..count {
        let share_id = block_height * 100000 + i as i64;

        sqlx::query(
            "INSERT INTO remote_shares (
                    id, origin, blockheight, workinfoid, clientid, enonce1, nonce2,
                    nonce, ntime, diff, sdiff, hash, result, workername, username,
                    createdate, createby, createcode, createinet, address
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
                )"
        )
            .bind(share_id)
            .bind("test_origin")
            .bind(block_height as i32)
            .bind(i as i64 + 1000)
            .bind(i as i64 + 100)
            .bind(format!("enonce1_{}", i))
            .bind(format!("nonce2_{}", i))
            .bind(format!("nonce_{}", i))
            .bind("507f1f77")
            .bind(1000.0 + i as f64)
            .bind(500.0 + i as f64)
            .bind(format!("hash_{:064x}", i))
            .bind(true)
            .bind(format!("worker_{}", i % 5))
            .bind(format!("{}", i % 10))
            .bind("2024-01-01 12:00:00")
            .bind("ckpool")
            .bind("")
            .bind("127.0.0.1")
            .bind(address(i % 10).to_string())
            .execute(&pool)
            .await?;
    }

    pool.close().await;
    Ok(())
}

async fn insert_test_shares_with_users(
    database_url: String,
    users: Vec<(String, f64)>,
    block_height: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    use sqlx::{Pool, Postgres, postgres::PgPoolOptions};

    let pool: Pool<Postgres> = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;

    let share_id_base = block_height * 10000;

    for (i, (username, diff)) in users.iter().enumerate() {
        let share_id = share_id_base + i as i64;

        sqlx::query(
            "INSERT INTO remote_shares (
                    id, origin, blockheight, workinfoid, clientid, enonce1, nonce2,
                    nonce, ntime, diff, sdiff, hash, result, workername, username,
                    createdate, createby, createcode, createinet
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19
                )"
        )
            .bind(share_id)
            .bind("test_origin")
            .bind(block_height as i32)
            .bind(1i64)
            .bind(1i64)
            .bind("enonce1")
            .bind("nonce2")
            .bind("nonce")
            .bind("ntime")
            .bind(diff)
            .bind(diff)
            .bind("hash")
            .bind(true)
            .bind(format!("{}_worker", username))
            .bind(username)
            .bind(chrono::Utc::now().to_rfc3339())
            .bind("test")
            .bind("test")
            .bind("127.0.0.1")
            .execute(&pool)
            .await?;
    }

    Ok(())
}

#[tokio::test]
async fn test_payouts_range_basic() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    for block_height in 100..=103 {
        insert_test_shares_remote(server.database_url().unwrap(), 50, block_height)
            .await
            .unwrap();
        insert_test_block(server.database_url().unwrap(), block_height)
            .await
            .unwrap();
    }

    let payouts: Vec<Payout> = server.get_json_async("/payouts/range/101/102").await;

    assert!(!payouts.is_empty());
    for payout in &payouts {
        assert!(payout.percentage > 0.0);
        assert!(payout.payable_shares > 0);
        assert!(payout.total_shares > 0);
    }
}

#[tokio::test]
async fn test_payouts_range_with_exclusions() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    for block_height in 200..=202 {
        insert_test_shares_with_users(
            server.database_url().unwrap(),
            vec![
                ("1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB".to_string(), 100.0),
                ("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX".to_string(), 200.0),
                (
                    "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string(),
                    300.0,
                ),
            ],
            block_height,
        )
        .await
        .unwrap();
        insert_test_block(server.database_url().unwrap(), block_height)
            .await
            .unwrap();
    }

    let payouts: Vec<Payout> = server
            .get_json_async("/payouts/range/200/202?excluded=3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX,bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
            .await;

    assert_eq!(payouts.len(), 1);
    assert_eq!(
        payouts[0].btcaddress,
        Some("1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB".to_string())
    );
    assert_eq!(payouts[0].percentage, 1.0);
}

#[tokio::test]
async fn test_payouts_range_empty_result() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let payouts: Vec<Payout> = server.get_json_async("/payouts/range/1000/1005").await;

    assert!(payouts.is_empty());
}

#[tokio::test]
async fn test_user_payout_range_basic() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    for block_height in 300..=303 {
        insert_test_shares_with_users(
            server.database_url().unwrap(),
            vec![
                ("1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB".to_string(), 150.0),
                ("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX".to_string(), 250.0),
                (
                    "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string(),
                    100.0,
                ),
            ],
            block_height,
        )
        .await
        .unwrap();
        insert_test_block(server.database_url().unwrap(), block_height)
            .await
            .unwrap();
    }

    let payouts: Vec<Payout> = server
        .get_json_async("/payouts/range/301/302/user/3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX")
        .await;

    assert_eq!(payouts.len(), 1);
    assert_eq!(
        payouts[0].btcaddress,
        Some("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX".to_string())
    );
    assert_eq!(payouts[0].percentage, 0.5);
}

#[tokio::test]
async fn test_user_payout_range_with_exclusions() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    for block_height in 400..=402 {
        insert_test_shares_with_users(
            server.database_url().unwrap(),
            vec![
                ("1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB".to_string(), 100.0),
                ("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX".to_string(), 200.0),
                (
                    "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string(),
                    300.0,
                ),
            ],
            block_height,
        )
        .await
        .unwrap();
        insert_test_block(server.database_url().unwrap(), block_height)
            .await
            .unwrap();
    }

    let payouts: Vec<Payout> = server
            .get_json_async("/payouts/range/400/402/user/1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB?excluded=bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
            .await;

    assert_eq!(payouts.len(), 1);
    assert_eq!(
        payouts[0].btcaddress,
        Some("1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB".to_string())
    );
    assert!((payouts[0].percentage - 0.333333).abs() < 0.01);
}

#[tokio::test]
async fn test_user_payout_range_excluded_user() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    for block_height in 500..=502 {
        insert_test_shares_with_users(
            server.database_url().unwrap(),
            vec![
                ("1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB".to_string(), 100.0),
                ("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX".to_string(), 200.0),
            ],
            block_height,
        )
        .await
        .unwrap();
        insert_test_block(server.database_url().unwrap(), block_height)
            .await
            .unwrap();
    }

    let payouts: Vec<Payout> = server
        .get_json_async(
            "/payouts/range/500/502/user/excluded?excluded=3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX",
        )
        .await;

    assert!(payouts.is_empty());
}

#[tokio::test]
async fn test_payouts_range_single_block() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    insert_test_shares_remote(server.database_url().unwrap(), 30, 600)
        .await
        .unwrap();
    insert_test_block(server.database_url().unwrap(), 600)
        .await
        .unwrap();

    let payouts: Vec<Payout> = server.get_json_async("/payouts/range/600/601").await;

    assert!(!payouts.is_empty());
}

#[tokio::test]
async fn test_payouts_range_large_range() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    for block_height in [700, 750, 800, 850, 900] {
        insert_test_shares_remote(server.database_url().unwrap(), 20, block_height)
            .await
            .unwrap();
        insert_test_block(server.database_url().unwrap(), block_height)
            .await
            .unwrap();
    }

    let payouts: Vec<Payout> = server.get_json_async("/payouts/range/700/900").await;

    assert!(!payouts.is_empty());
    let total_percentage: f64 = payouts.iter().map(|p| p.percentage).sum();
    assert!((total_percentage - 1.0).abs() < 0.01);
}

#[tokio::test]
async fn test_payouts_range_url_encoded_exclusions() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    for block_height in 1000..=1002 {
        insert_test_shares_with_users(
            server.database_url().unwrap(),
            vec![
                ("1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB".to_string(), 100.0),
                ("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX".to_string(), 200.0),
                (
                    "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string(),
                    300.0,
                ),
            ],
            block_height,
        )
        .await
        .unwrap();
        insert_test_block(server.database_url().unwrap(), block_height)
            .await
            .unwrap();
    }

    let encoded_exclusions = urlencoding::encode(
        "1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB,3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX",
    );
    let payouts: Vec<Payout> = server
        .get_json_async(&format!(
            "/payouts/range/1000/1002?excluded={}",
            encoded_exclusions
        ))
        .await;

    assert_eq!(payouts.len(), 1);
    assert_eq!(
        payouts[0].btcaddress,
        Some("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string())
    );
}

#[tokio::test]
async fn test_invalid() {
    let server = TestServer::spawn_with_db_args("--admin-token verysecrettoken").await;

    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let res = server.get_json_async_raw("/split").await;
    assert!(!res.status().is_success());
}

#[tokio::test]
async fn test_payouts_content_negotiation() {
    let mut server = TestServer::spawn_with_db_args("--admin-token verysecrettoken").await;

    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    // fail requests without auth
    let res = server.get_json_async_raw("/payouts").await;
    assert!(!res.status().is_success());
    let res = server.get_json_async_raw("/payouts?format=json").await;
    assert!(!res.status().is_success());

    server.admin_token = Some("verysecrettoken".into());
    let res = server.get_json_async_raw("/payouts?format=json").await;
    assert!(res.status().is_success());
    let content_type = res.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(content_type.contains("application/json"));
}

#[tokio::test]
async fn test_valid_auth() {
    let mut server = TestServer::spawn_with_db_args("--admin-token verysecrettoken").await;

    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    server.admin_token = Some("verysecrettoken".into());

    let res = server.get_json_async_raw("/split").await;
    assert!(res.status().is_success());
}

#[tokio::test]
async fn test_highestdiff_basic() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    insert_test_shares_with_diff(
        server.database_url().unwrap(),
        vec![
            (
                "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string(),
                1000.0,
            ),
            ("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX".to_string(), 5000.0),
            ("1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB".to_string(), 2500.0),
        ],
        100,
    )
    .await
    .unwrap();

    let highestdiff: HighestDiff = server.get_json_async("/highestdiff/100").await;

    assert_eq!(highestdiff.blockheight, 100);
    assert_eq!(highestdiff.username, "3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX");
    assert_eq!(highestdiff.diff, 5000.0);
}

#[tokio::test]
async fn test_highestdiff_not_found() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let res = server.get_json_async_raw("/highestdiff/999").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_highestdiff_by_user_basic() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    insert_test_shares_with_diff(
        server.database_url().unwrap(),
        vec![
            (
                "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string(),
                1000.0,
            ),
            (
                "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string(),
                3000.0,
            ),
            ("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX".to_string(), 5000.0),
        ],
        200,
    )
    .await
    .unwrap();

    let highestdiff: HighestDiff = server
        .get_json_async("/highestdiff/200/user/bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
        .await;

    assert_eq!(highestdiff.blockheight, 200);
    assert_eq!(
        highestdiff.username,
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
    );
    assert_eq!(highestdiff.diff, 3000.0);
}

#[tokio::test]
async fn test_highestdiff_by_user_not_found() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    insert_test_shares_with_diff(
        server.database_url().unwrap(),
        vec![(
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string(),
            1000.0,
        )],
        300,
    )
    .await
    .unwrap();

    let res = server
        .get_json_async_raw("/highestdiff/300/user/nonexistent")
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_highestdiff_all_users_basic() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    insert_test_shares_with_diff(
        server.database_url().unwrap(),
        vec![
            (
                "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string(),
                1000.0,
            ),
            (
                "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".to_string(),
                2000.0,
            ),
            ("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX".to_string(), 3000.0),
            ("3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX".to_string(), 1500.0),
            ("1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB".to_string(), 500.0),
        ],
        400,
    )
    .await
    .unwrap();

    let highestdiffs: Vec<HighestDiff> = server.get_json_async("/highestdiff/400/all").await;

    assert_eq!(highestdiffs.len(), 3);

    let user_a = highestdiffs
        .iter()
        .find(|h| h.username == "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
        .unwrap();
    assert_eq!(user_a.diff, 2000.0);

    let user_b = highestdiffs
        .iter()
        .find(|h| h.username == "3EktnHQD7RiAE6uzMj2ZifT9YgRrkSgzQX")
        .unwrap();
    assert_eq!(user_b.diff, 3000.0);

    let user_c = highestdiffs
        .iter()
        .find(|h| h.username == "1CPDJtMzuSyvnGi8o9ZAtAWPfqHZhjQQhB")
        .unwrap();
    assert_eq!(user_c.diff, 500.0);
}

#[tokio::test]
async fn test_highestdiff_all_users_empty() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let highestdiffs: Vec<HighestDiff> = server.get_json_async("/highestdiff/999/all").await;

    assert!(highestdiffs.is_empty());
}

#[tokio::test]
async fn aggregator_blockheight_no_nodes() {
    let server = TestServer::spawn_with_db().await;

    let blockheight_response = server.get_json_async_raw("/aggregator/blockheight").await;
    assert_eq!(
        blockheight_response.status(),
        StatusCode::NOT_FOUND,
        "Should not find records when no nodes are configured"
    );
}

#[tokio::test]
async fn aggregator_blockheight_returns_minimum() {
    let low_node = TestServer::spawn_with_db_args("--admin-token admin_token").await;
    fs::write(low_node.tempdir.path().join("current_id.txt"), "1").unwrap();
    let source_db_url = low_node.database_url().unwrap();
    setup_test_schema(source_db_url.clone()).await.unwrap();
    insert_test_shares(source_db_url.clone(), 1, 800000)
        .await
        .unwrap();

    let high_node = TestServer::spawn_with_db_args("--admin-token admin_token").await;
    fs::write(high_node.tempdir.path().join("current_id.txt"), "1").unwrap();
    let source_db_url = high_node.database_url().unwrap();
    setup_test_schema(source_db_url.clone()).await.unwrap();
    insert_test_shares(source_db_url.clone(), 1, 800001)
        .await
        .unwrap();

    let aggregator = TestServer::spawn_with_db_args(format!(
        "--nodes {} --nodes {} --api-token aggregator_api_token --admin-token admin_token",
        low_node.url(),
        high_node.url()
    ))
    .await;
    let aggregator_db_url = aggregator.database_url().unwrap();
    setup_test_schema(aggregator_db_url.clone()).await.unwrap();

    let client = reqwest::Client::new();
    let response = client
        .get(
            aggregator
                .url()
                .join("/aggregator/blockheight".as_ref())
                .unwrap(),
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .bearer_auth("aggregator_api_token")
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "Response: {}",
        response.text().await.unwrap()
    );

    let blockheight: i32 = response.json().await.unwrap();

    assert_eq!(blockheight, 800000);
}

#[tokio::test]
async fn test_payouts_simulate_basic() {
    let mut server = TestServer::spawn_with_db_args("--admin-token testtoken").await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    insert_test_account_with_diff(
        server.database_url().unwrap(),
        "user1",
        Some("user1@ln.test"),
        1000,
    )
    .await
    .unwrap();

    insert_test_account_with_diff(
        server.database_url().unwrap(),
        "user2",
        Some("user2@ln.test"),
        3000,
    )
    .await
    .unwrap();

    server.admin_token = Some("testtoken".into());
    let payouts: Vec<PendingPayout> = server
        .get_json_async_raw("/payouts/simulate?format=json")
        .await
        .json()
        .await
        .unwrap();

    assert_eq!(payouts.len(), 2);

    let total_amount: i64 = payouts.iter().map(|p| p.amount_sats).sum();
    assert!(total_amount <= 312_500_000);

    let user2_payout = payouts.iter().find(|p| p.btc_address == "user2").unwrap();
    let user1_payout = payouts.iter().find(|p| p.btc_address == "user1").unwrap();
    assert!(user2_payout.amount_sats > user1_payout.amount_sats);
}

#[tokio::test]
async fn test_payouts_simulate_excludes_already_paid() {
    let mut server = TestServer::spawn_with_db_args("--admin-token testtoken").await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    insert_test_account_with_diff(
        server.database_url().unwrap(),
        "user1",
        Some("user1@ln.test"),
        1000,
    )
    .await
    .unwrap();

    insert_test_payout(
        server.database_url().unwrap(),
        "user1",
        50000,
        500,
        "success",
    )
    .await
    .unwrap();

    insert_test_account_with_diff(
        server.database_url().unwrap(),
        "user2",
        Some("user2@ln.test"),
        1000,
    )
    .await
    .unwrap();

    server.admin_token = Some("testtoken".into());
    let payouts: Vec<PendingPayout> = server
        .get_json_async_raw("/payouts/simulate?format=json")
        .await
        .json()
        .await
        .unwrap();

    assert_eq!(payouts.len(), 2);

    let user1_payout = payouts.iter().find(|p| p.btc_address == "user1").unwrap();
    let user2_payout = payouts.iter().find(|p| p.btc_address == "user2").unwrap();

    assert!(user2_payout.amount_sats > user1_payout.amount_sats);
}

#[tokio::test]
async fn test_payouts_simulate_empty_when_fully_paid() {
    let mut server = TestServer::spawn_with_db_args("--admin-token testtoken").await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    insert_test_account_with_diff(
        server.database_url().unwrap(),
        "user1",
        Some("user1@ln.test"),
        1000,
    )
    .await
    .unwrap();

    insert_test_payout(
        server.database_url().unwrap(),
        "user1",
        50000,
        1000,
        "success",
    )
    .await
    .unwrap();

    server.admin_token = Some("testtoken".into());
    let payouts: Vec<PendingPayout> = server
        .get_json_async_raw("/payouts/simulate?format=json")
        .await
        .json()
        .await
        .unwrap();

    assert!(payouts.is_empty());
}

#[tokio::test]
async fn test_payouts_simulate_empty_database() {
    let mut server = TestServer::spawn_with_db_args("--admin-token testtoken").await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    server.admin_token = Some("testtoken".into());
    let payouts: Vec<PendingPayout> = server
        .get_json_async_raw("/payouts/simulate?format=json")
        .await
        .json()
        .await
        .unwrap();

    assert!(payouts.is_empty());
}

#[tokio::test]
async fn test_payouts_simulate_requires_auth() {
    let server = TestServer::spawn_with_db_args("--admin-token testtoken").await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let res = server
        .get_json_async_raw("/payouts/simulate?format=json")
        .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_payouts_simulate_groups_by_lnurl() {
    let mut server = TestServer::spawn_with_db_args("--admin-token testtoken").await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    insert_test_account_with_diff(
        server.database_url().unwrap(),
        "user1",
        Some("shared@ln.test"),
        1000,
    )
    .await
    .unwrap();

    insert_test_account_with_diff(
        server.database_url().unwrap(),
        "user2",
        Some("shared@ln.test"),
        1000,
    )
    .await
    .unwrap();

    insert_test_account_with_diff(
        server.database_url().unwrap(),
        "user3",
        Some("other@ln.test"),
        2000,
    )
    .await
    .unwrap();

    server.admin_token = Some("testtoken".into());
    let payouts: Vec<PendingPayout> = server
        .get_json_async_raw("/payouts/simulate?format=json")
        .await
        .json()
        .await
        .unwrap();

    assert_eq!(payouts.len(), 2);

    let shared_payout = payouts
        .iter()
        .find(|p| p.ln_address == "shared@ln.test")
        .unwrap();
    let other_payout = payouts
        .iter()
        .find(|p| p.ln_address == "other@ln.test")
        .unwrap();

    assert_eq!(shared_payout.amount_sats, other_payout.amount_sats);
}

#[tokio::test]
async fn test_rounds_empty() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let rounds: Vec<Round> = server.get_json_async("/rounds").await;
    assert!(rounds.is_empty());
}

#[tokio::test]
async fn test_rounds_lists_found_blocks() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_block(db_url.clone(), 100).await.unwrap();
    insert_test_block(db_url.clone(), 200).await.unwrap();
    insert_test_block(db_url.clone(), 300).await.unwrap();

    let rounds: Vec<Round> = server.get_json_async("/rounds").await;
    assert_eq!(rounds.len(), 3);
    assert_eq!(rounds[0].blockheight, 100);
    assert_eq!(rounds[1].blockheight, 200);
    assert_eq!(rounds[2].blockheight, 300);
    assert!(!rounds[0].blockhash.is_empty());
}

#[tokio::test]
async fn test_round_participants() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_block(db_url.clone(), 5).await.unwrap();
    insert_test_block(db_url.clone(), 10).await.unwrap();

    insert_test_shares_for_round(
        db_url.clone(),
        vec![("foo", 1000.0), ("bar", 5000.0)],
        6,
        1000,
    )
    .await
    .unwrap();
    insert_test_shares_for_round(
        db_url.clone(),
        vec![("foo", 3000.0), ("baz", 2000.0)],
        8,
        2000,
    )
    .await
    .unwrap();

    let participants: Vec<RoundParticipant> = server.get_json_async("/rounds/10").await;
    assert_eq!(participants.len(), 3);

    let foo = participants.iter().find(|p| p.username == "foo").unwrap();
    assert_eq!(foo.blocks_participated, 2);
    assert_eq!(foo.top_diff, 3000.0);
    // total_work sums accepted diff across the round (1000 @ bh6 + 3000 @ bh8)
    assert_eq!(foo.total_work, 4000.0);

    let bar = participants.iter().find(|p| p.username == "bar").unwrap();
    assert_eq!(bar.blocks_participated, 1);
    assert_eq!(bar.top_diff, 5000.0);
    assert_eq!(bar.total_work, 5000.0);

    let baz = participants.iter().find(|p| p.username == "baz").unwrap();
    assert_eq!(baz.blocks_participated, 1);
    assert_eq!(baz.top_diff, 2000.0);
    assert_eq!(baz.total_work, 2000.0);
}

#[tokio::test]
async fn test_round_excludes_shares_from_previous_round() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_block(db_url.clone(), 5).await.unwrap();
    insert_test_block(db_url.clone(), 10).await.unwrap();

    insert_test_shares_for_round(db_url.clone(), vec![("foo", 9000.0)], 4, 500)
        .await
        .unwrap();
    insert_test_shares_for_round(db_url.clone(), vec![("bar", 1000.0)], 7, 600)
        .await
        .unwrap();

    let participants: Vec<RoundParticipant> = server.get_json_async("/rounds/10").await;
    assert_eq!(participants.len(), 1);
    assert_eq!(participants[0].username, "bar");
}

#[tokio::test]
async fn test_round_first_includes_all_prior_shares() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_block(db_url.clone(), 5).await.unwrap();

    insert_test_shares_for_round(db_url.clone(), vec![("foo", 100.0)], 1, 100)
        .await
        .unwrap();
    insert_test_shares_for_round(db_url.clone(), vec![("foo", 200.0)], 3, 200)
        .await
        .unwrap();
    insert_test_shares_for_round(db_url.clone(), vec![("foo", 300.0)], 5, 300)
        .await
        .unwrap();

    let participants: Vec<RoundParticipant> = server.get_json_async("/rounds/5").await;
    assert_eq!(participants.len(), 1);
    assert_eq!(participants[0].blocks_participated, 3);
    assert_eq!(participants[0].top_diff, 300.0);
}

#[tokio::test]
async fn test_round_empty() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let participants: Vec<RoundParticipant> = server.get_json_async("/rounds/999").await;
    assert!(participants.is_empty());
}

#[tokio::test]
async fn test_current_round() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_block(db_url.clone(), 5).await.unwrap();

    insert_test_shares_for_round(db_url.clone(), vec![("foo", 1000.0)], 3, 100)
        .await
        .unwrap();
    insert_test_shares_for_round(
        db_url.clone(),
        vec![("foo", 2000.0), ("bar", 500.0)],
        7,
        200,
    )
    .await
    .unwrap();

    refresh_round_participation_view(db_url).await.unwrap();

    let participants: Vec<RoundParticipant> = server.get_json_async("/rounds/current").await;
    assert_eq!(participants.len(), 2);

    let foo = participants.iter().find(|p| p.username == "foo").unwrap();
    assert_eq!(foo.blocks_participated, 1);
    assert_eq!(foo.top_diff, 2000.0);
    // only the current round's share (2000 @ bh7) counts; bh3 is a prior round
    assert_eq!(foo.total_work, 2000.0);

    let bar = participants.iter().find(|p| p.username == "bar").unwrap();
    assert_eq!(bar.blocks_participated, 1);
    assert_eq!(bar.top_diff, 500.0);
    assert_eq!(bar.total_work, 500.0);
}

#[tokio::test]
async fn test_current_round_no_blocks_found() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_shares_for_round(db_url.clone(), vec![("foo", 100.0), ("bar", 200.0)], 1, 100)
        .await
        .unwrap();

    refresh_round_participation_view(db_url).await.unwrap();

    let participants: Vec<RoundParticipant> = server.get_json_async("/rounds/current").await;
    assert_eq!(participants.len(), 2);
}

#[tokio::test]
async fn test_participants_for_blockheight() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_shares_for_round(
        db_url.clone(),
        vec![("foo", 100.0), ("bar", 200.0), ("baz", 300.0)],
        42,
        100,
    )
    .await
    .unwrap();
    insert_test_shares_for_round(db_url.clone(), vec![("other", 100.0)], 43, 200)
        .await
        .unwrap();

    let participants: Vec<String> = server.get_json_async("/participants/42").await;
    assert_eq!(participants.len(), 3);
    assert!(participants.contains(&"foo".to_string()));
    assert!(participants.contains(&"bar".to_string()));
    assert!(participants.contains(&"baz".to_string()));
    assert!(!participants.contains(&"other".to_string()));
}

#[tokio::test]
async fn test_participants_empty() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let participants: Vec<String> = server.get_json_async("/participants/999").await;
    assert!(participants.is_empty());
}

#[tokio::test]
async fn test_participants_excludes_rejected_shares() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let pool = sqlx::PgPool::connect(&db_url).await.unwrap();

    sqlx::query(
        "INSERT INTO remote_shares (
            id, origin, blockheight, diff, sdiff, result, workername, username,
            createdate, createby, createcode, createinet, reject_reason
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(900i64)
    .bind("test_origin")
    .bind(50i32)
    .bind(100.0)
    .bind(100.0)
    .bind(false)
    .bind("foo_worker")
    .bind("foo")
    .bind("2024-01-01 12:00:00")
    .bind("test")
    .bind("test")
    .bind("127.0.0.1")
    .bind("stale")
    .execute(&pool)
    .await
    .unwrap();

    insert_test_shares_for_round(db_url.clone(), vec![("bar", 100.0)], 50, 901)
        .await
        .unwrap();

    pool.close().await;

    let participants: Vec<String> = server.get_json_async("/participants/50").await;
    assert_eq!(participants.len(), 1);
    assert_eq!(participants[0], "bar");
}

#[tokio::test]
async fn auth_tiers_with_db() {
    let server = TestServer::spawn_with_db_args("--api-token foo --admin-token bar").await;

    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    async fn case(server: &TestServer, path: &str, token: &str, expected: StatusCode) {
        let response = reqwest::Client::new()
            .get(server.url().join(path).unwrap())
            .bearer_auth(token)
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), expected, "{path} with token {token}");
    }

    case(&server, "/rounds", "foo", StatusCode::OK).await;
    case(&server, "/rounds", "bar", StatusCode::OK).await;
    case(&server, "/payouts", "foo", StatusCode::UNAUTHORIZED).await;
    case(&server, "/payouts", "bar", StatusCode::OK).await;
}

#[derive(Deserialize, Debug)]
struct BadgeInstance {
    blockheight: i32,
}

#[derive(Deserialize, Debug)]
struct BadgeBucket {
    count: i64,
}

#[derive(Deserialize, Debug)]
struct BadgeType {
    #[allow(dead_code)]
    kind: String,
    unique: Vec<BadgeInstance>,
    bucket: BadgeBucket,
    total: i64,
}

#[derive(Deserialize, Debug)]
struct BadgesPayload {
    version: u32,
    types: std::collections::HashMap<String, BadgeType>,
}

#[derive(Deserialize, Debug)]
struct BadgeDefinition {
    id: String,
    unique_cap: Option<i64>,
}

/// Record a found block at each height and have `foo` submit one accepted share
/// AT that exact height, so `foo` earns a "mined on block" badge for each.
async fn seed_block_participation(server: &TestServer, heights: &[i64]) {
    let db_url = server.database_url().unwrap();

    for (i, h) in heights.iter().enumerate() {
        insert_test_block(db_url.clone(), *h).await.unwrap();
        insert_test_shares_for_round(
            db_url.clone(),
            vec![("foo", 1000.0)],
            *h,
            1000 + (i as i64) * 100,
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn test_badges_three_unique_no_bucket() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();
    insert_test_account_with_diff(db_url.clone(), "foo", None, 0)
        .await
        .unwrap();

    seed_block_participation(&server, &[10, 20, 30]).await;

    let payload: BadgesPayload = server.get_json_async("/badges/foo").await;
    assert_eq!(payload.version, 1);

    let block = payload.types.get("block").unwrap();
    assert_eq!(block.total, 3);
    assert_eq!(block.bucket.count, 0);

    let heights: Vec<i32> = block.unique.iter().map(|u| u.blockheight).collect();
    assert_eq!(heights, vec![10, 20, 30]);
}

#[tokio::test]
async fn test_badges_stacked_bucket() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();
    insert_test_account_with_diff(db_url.clone(), "foo", None, 0)
        .await
        .unwrap();

    seed_block_participation(&server, &[10, 20, 30, 40, 50]).await;

    let payload: BadgesPayload = server.get_json_async("/badges/foo").await;
    let block = payload.types.get("block").unwrap();

    // 5 blocks => earliest 3 unique medals + a "+2" stacking bucket.
    assert_eq!(block.total, 5);
    assert_eq!(block.unique.len(), 3);
    assert_eq!(block.bucket.count, 2);

    let heights: Vec<i32> = block.unique.iter().map(|u| u.blockheight).collect();
    assert_eq!(heights, vec![10, 20, 30]);
}

#[tokio::test]
async fn test_badges_account_without_blocks_is_empty() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();
    insert_test_account_with_diff(db_url.clone(), "foo", None, 0)
        .await
        .unwrap();

    let payload: BadgesPayload = server.get_json_async("/badges/foo").await;
    let block = payload.types.get("block").unwrap();
    assert_eq!(block.total, 0);
    assert!(block.unique.is_empty());
    assert_eq!(block.bucket.count, 0);
}

#[tokio::test]
async fn test_badges_unknown_account_is_404() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let response = server.get_json_async_raw("/badges/nobody").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_badges_catalog() {
    let server = TestServer::spawn_with_db().await;
    setup_test_schema(server.database_url().unwrap())
        .await
        .unwrap();

    let catalog: Vec<BadgeDefinition> = server.get_json_async("/badges").await;
    let block = catalog.iter().find(|d| d.id == "block").unwrap();
    assert_eq!(block.unique_cap, Some(3));
}

/// Set `account_metadata.data.block_count` for a user (loyalty badge source).
async fn set_block_count(database_url: String, username: &str, block_count: i64) {
    let pool = sqlx::PgPool::connect(&database_url).await.unwrap();
    sqlx::query(
        "
        INSERT INTO account_metadata (account_id, data, created_at, updated_at)
        SELECT id, jsonb_build_object('block_count', $2::bigint), NOW(), NOW()
        FROM accounts WHERE username = $1
        ON CONFLICT (account_id) DO UPDATE
        SET data = account_metadata.data || jsonb_build_object('block_count', $2::bigint)
        ",
    )
    .bind(username)
    .bind(block_count)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn test_badges_block_winner() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();
    // insert_test_block records "test_user" as the winning username.
    insert_test_account_with_diff(db_url.clone(), "test_user", None, 0)
        .await
        .unwrap();
    insert_test_block(db_url.clone(), 10).await.unwrap();
    insert_test_block(db_url.clone(), 20).await.unwrap();

    let payload: BadgesPayload = server.get_json_async("/badges/test_user").await;
    let winner = payload.types.get("block_winner").unwrap();

    assert_eq!(winner.total, 2);
    assert_eq!(winner.bucket.count, 0);
    let heights: Vec<i32> = winner.unique.iter().map(|u| u.blockheight).collect();
    assert_eq!(heights, vec![10, 20]);
}

#[tokio::test]
async fn test_badges_loyalty() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();
    insert_test_account_with_diff(db_url.clone(), "foo", None, 0)
        .await
        .unwrap();
    // 25_000 blocks => floor(25000 / 10000) = 2 loyalty instances.
    set_block_count(db_url.clone(), "foo", 25_000).await;

    let payload: BadgesPayload = server.get_json_async("/badges/foo").await;
    let loyalty = payload.types.get("loyalty").unwrap();

    assert_eq!(loyalty.total, 2);
    assert_eq!(loyalty.bucket.count, 2);
    assert!(loyalty.unique.is_empty());
}

#[tokio::test]
async fn test_badges_block_requires_exact_height() {
    // Block found at height 20. `foo` submits a share at 15 (in the round but
    // not on the block); `bar` submits at exactly 20. Only `bar` earns a badge.
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();
    insert_test_account_with_diff(db_url.clone(), "foo", None, 0)
        .await
        .unwrap();
    insert_test_account_with_diff(db_url.clone(), "bar", None, 0)
        .await
        .unwrap();
    insert_test_block(db_url.clone(), 20).await.unwrap();
    insert_test_shares_for_round(db_url.clone(), vec![("foo", 1000.0)], 15, 3000)
        .await
        .unwrap();
    insert_test_shares_for_round(db_url.clone(), vec![("bar", 1000.0)], 20, 3100)
        .await
        .unwrap();

    let foo: BadgesPayload = server.get_json_async("/badges/foo").await;
    assert_eq!(foo.types.get("block").unwrap().total, 0);

    let bar: BadgesPayload = server.get_json_async("/badges/bar").await;
    let block = bar.types.get("block").unwrap();
    assert_eq!(block.total, 1);
    let heights: Vec<i32> = block.unique.iter().map(|u| u.blockheight).collect();
    assert_eq!(heights, vec![20]);
}

#[tokio::test]
async fn test_badges_cache_invalidates_on_new_block() {
    // First read caches the payload with the current found-block tip. When a new
    // block is found (tip advances) and foo mined it, the next read recomputes
    // rather than serving stale cache.
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();
    insert_test_account_with_diff(db_url.clone(), "foo", None, 0)
        .await
        .unwrap();

    insert_test_block(db_url.clone(), 10).await.unwrap();
    insert_test_shares_for_round(db_url.clone(), vec![("foo", 1000.0)], 10, 1000)
        .await
        .unwrap();

    let first: BadgesPayload = server.get_json_async("/badges/foo").await;
    assert_eq!(first.types.get("block").unwrap().total, 1);

    // A new found block foo also mined.
    insert_test_block(db_url.clone(), 20).await.unwrap();
    insert_test_shares_for_round(db_url.clone(), vec![("foo", 1000.0)], 20, 2000)
        .await
        .unwrap();

    let second: BadgesPayload = server.get_json_async("/badges/foo").await;
    assert_eq!(second.types.get("block").unwrap().total, 2);
}

/// Write a badges payload directly into `account_metadata.data.badges`.
async fn seed_cached_badges(database_url: String, username: &str, badges: serde_json::Value) {
    let pool = sqlx::PgPool::connect(&database_url).await.unwrap();
    sqlx::query(
        "
        INSERT INTO account_metadata (account_id, data, created_at, updated_at)
        SELECT id, jsonb_build_object('badges', $2::jsonb), NOW(), NOW()
        FROM accounts WHERE username = $1
        ON CONFLICT (account_id) DO UPDATE
        SET data = account_metadata.data || jsonb_build_object('badges', $2::jsonb)
        ",
    )
    .bind(username)
    .bind(badges)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn test_badges_external_outage_keeps_cached_value() {
    // Router configured but unreachable. A recompute must preserve the
    // previously cached refinery badge rather than dropping it.
    let server = TestServer::spawn_with_db_args("--router-url http://127.0.0.1:1").await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();
    insert_test_account_with_diff(db_url.clone(), "foo", None, 0)
        .await
        .unwrap();

    // Stale cache (source_tip/source_blocks won't match) that holds a refinery badge.
    seed_cached_badges(
        db_url.clone(),
        "foo",
        serde_json::json!({
            "version": 1,
            "computed_at": "2020-01-01T00:00:00Z",
            "source_tip": -1,
            "source_blocks": -1,
            "types": {
                "refinery": {
                    "kind": "bucket",
                    "unique": [],
                    "bucket": { "count": 7 },
                    "total": 7
                }
            }
        }),
    )
    .await;

    let payload: BadgesPayload = server.get_json_async("/badges/foo").await;

    let refinery = payload
        .types
        .get("refinery")
        .expect("refinery badge should be carried forward through an outage");
    assert_eq!(refinery.total, 7);
}
