use super::*;

#[test]
fn pool_status_zero() {
    let server = TestServer::spawn();

    fs::write(
        server.log_dir().join("pool/pool.status"),
        zero_status().to_string(),
    )
    .unwrap();

    server.assert_response("/pool/pool.status", &zero_status().to_string());
}

#[test]
fn pool_status_typical() {
    let server = TestServer::spawn();

    fs::write(
        server.log_dir().join("pool/pool.status"),
        typical_status().to_string(),
    )
    .unwrap();

    server.assert_response("/pool/pool.status", &typical_status().to_string());
}

#[test]
fn user_status_zero() {
    let server = TestServer::spawn();
    let user = zero_user();
    let user_address = address(0);

    let user_str = serde_json::to_string(&user).unwrap();

    fs::write(
        server.log_dir().join(format!("users/{user_address}")),
        &user_str,
    )
    .unwrap();

    server.assert_response(format!("/users/{user_address}"), &user_str);
}

#[test]
fn user_status_typical() {
    let server = TestServer::spawn();
    let user = typical_user();
    let user_address = address(0);

    let user_str = serde_json::to_string(&user).unwrap();

    fs::write(
        server.log_dir().join(format!("users/{user_address}")),
        &user_str,
    )
    .unwrap();

    server.assert_response(format!("/users/{user_address}"), &user_str);
}

#[test]
fn list_users() {
    let server = TestServer::spawn();
    let mut users = BTreeMap::new();
    for i in 0..9 {
        let user = typical_user();
        let user_address = address(i);
        let user_str = serde_json::to_string(&user).unwrap();

        users.insert(user_address.to_string(), user);

        fs::write(
            server.log_dir().join(format!("users/{user_address}")),
            &user_str,
        )
        .unwrap();
    }

    let users_response = server.get_json::<Vec<String>>("/users");

    assert_eq!(users_response.len(), users.len());
    assert_eq!(
        users_response.into_iter().collect::<HashSet<String>>(),
        users.into_keys().collect::<HashSet<String>>()
    );
}

#[test]
fn aggregate_pool_status() {
    let mut servers = Vec::new();
    for _ in 0..3 {
        let server = TestServer::spawn();
        fs::write(
            server.log_dir().join("pool/pool.status"),
            typical_status().to_string(),
        )
        .unwrap();

        servers.push(server)
    }

    assert_eq!(servers.len(), 3);

    let aggregator = TestServer::spawn_with_args(format!(
        "--nodes {} --nodes {} --nodes {}",
        servers[0].url(),
        servers[1].url(),
        servers[2].url()
    ));

    aggregator.assert_response(
        "/aggregator/pool/pool.status",
        &(typical_status() + typical_status() + typical_status()).to_string(),
    );
}

#[test]
fn aggregate_users() {
    let mut users = Vec::new();
    for i in 0..9 {
        let user = typical_user();
        let user_address = address(i);

        users.push((user_address.to_string(), user));
    }

    assert_eq!(users.len(), 9);

    let mut servers = Vec::new();
    for (address, user) in users.iter().take(3) {
        let server = TestServer::spawn();

        fs::write(
            server.log_dir().join(format!("users/{address}")),
            serde_json::to_string(&user).unwrap(),
        )
        .unwrap();

        servers.push(server)
    }

    assert_eq!(servers.len(), 3);

    let aggregator = TestServer::spawn_with_args(format!(
        "--nodes {} --nodes {} --nodes {}",
        servers[0].url(),
        servers[1].url(),
        servers[2].url()
    ));

    for (address, user) in users.iter().take(3) {
        let response = aggregator.get_json::<User>(format!("/aggregator/users/{address}"));
        pretty_assert_eq!(response, *user);
    }
}

#[test]
fn healthcheck_json() {
    let server = TestServer::spawn();

    let healthcheck = server.get_json::<Healthcheck>("/healthcheck");

    assert!(healthcheck.disk_usage_percent > 0.0);
}

#[test]
fn healthcheck_with_auth() {
    let server = TestServer::spawn_with_args("--username foo --password bar");

    let response = reqwest::blocking::Client::new()
        .get(format!("{}healthcheck", server.url()))
        .send()
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = reqwest::blocking::Client::new()
        .get(format!("{}healthcheck", server.url()))
        .basic_auth("foo", Some("bar"))
        .send()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn aggregator_dashboard_with_auth() {
    let mut servers = Vec::new();
    for _ in 0..3 {
        let server = TestServer::spawn_with_args("--username foo --password bar");
        servers.push(server)
    }

    assert_eq!(servers.len(), 3);

    let aggregator = TestServer::spawn_with_args(format!(
        "--username foo --password bar --nodes {} --nodes {} --nodes {}",
        servers[0].url(),
        servers[1].url(),
        servers[2].url()
    ));

    let response = reqwest::blocking::Client::new()
        .get(format!("{}aggregator/dashboard", aggregator.url()))
        .send()
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = reqwest::blocking::Client::new()
        .get(format!("{}aggregator/dashboard", aggregator.url()))
        .basic_auth("foo", Some("bar"))
        .send()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// #[cfg(target_os = "linux")]
mod payout_range_tests {
    use super::*;
    use crate::test_psql::{insert_test_block, setup_test_schema};
    use para::subcommand::server::database::Payout;

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

        let payouts: Vec<Payout> = server.get_json_async("/payouts/range/600/600").await;

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
}

// #[cfg(target_os = "linux")]
mod auth_tests {
    use super::*;
    use crate::test_psql::setup_test_schema;
    use crate::test_server::Credentials;

    #[tokio::test]
    async fn test_invalid_auth() {
        let server =
            TestServer::spawn_with_db_args("--username test_user --password test_pass").await;

        setup_test_schema(server.database_url().unwrap())
            .await
            .unwrap();

        let res: Response = server.get_json_async_raw("/split").await;
        assert!(!res.status().is_success());
    }

    #[tokio::test]
    async fn test_valid_auth() {
        let mut server =
            TestServer::spawn_with_db_args("--username test_user --password test_pass").await;

        setup_test_schema(server.database_url().unwrap())
            .await
            .unwrap();

        server.credentials = Some(Credentials {
            username: "test_user".into(),
            password: "test_pass".into(),
        });
        let res: Response = server.get_json_async_raw("/split").await;
        assert!(res.status().is_success());
    }
}
