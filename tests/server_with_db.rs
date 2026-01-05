use super::*;

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
    use crate::address;
    use sqlx::{Pool, Postgres, postgres::PgPoolOptions};

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
    let node1 = TestServer::spawn_with_db_args("--admin-token token1").await;
    let node2 = TestServer::spawn_with_db_args("--admin-token token2").await;

    let aggregator = TestServer::spawn_with_db_args(format!(
        "--nodes {} --nodes {} --admin-token aggregator_token",
        node1.url(),
        node2.url()
    ));

    let blockheight: i32 = aggregator
        .await
        .get_json_async("/aggregator/blockheight")
        .await;

    assert!(blockheight >= 0);
}
