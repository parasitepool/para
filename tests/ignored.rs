use super::*;

// These tests either call some scripts that are not available in CI yet or are
// a bit too expensive so marking them as ignored for now.

#[test]
#[ignore]
fn miner() {
    let pool = TestPool::spawn();

    let bitcoind = pool.bitcoind_handle();

    bitcoind.mine_blocks(16).unwrap();

    let stratum_endpoint = pool.stratum_endpoint();

    let miner = CommandBuilder::new(format!(
        "miner --once --username {} {stratum_endpoint}",
        signet_username()
    ))
    .spawn();

    let stdout = miner.wait_with_output().unwrap();
    let output =
        serde_json::from_str::<Vec<Share>>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    assert_eq!(output.len(), 1);
}

#[test]
#[ignore]
fn concurrently_listening_workers_receive_new_templates_on_new_block() {
    let pool = TestPool::spawn();
    let endpoint = pool.stratum_endpoint();
    let user = signet_username();

    let gate = Arc::new(Barrier::new(3));
    let (out_1, in_1) = mpsc::channel();
    let (out_2, in_2) = mpsc::channel();

    thread::scope(|thread| {
        for out in [out_1.clone(), out_2.clone()].into_iter() {
            let gate = gate.clone();
            let endpoint = endpoint.clone();
            let user = user.clone();

            thread.spawn(move || {
                let mut template_watcher =
                    CommandBuilder::new(format!("template {endpoint} --username {user} --watch"))
                        .spawn();

                let mut reader = BufReader::new(template_watcher.stdout.take().unwrap());

                let initial_template = next_json::<Template>(&mut reader);

                gate.wait();

                let new_template = next_json::<Template>(&mut reader);

                out.send((initial_template, new_template)).ok();

                template_watcher.kill().unwrap();
                template_watcher.wait().unwrap();
            });
        }

        gate.wait();

        pool.bitcoind_handle().mine_blocks(1).unwrap();

        let (initial_template_worker_a, new_template_worker_a) =
            in_1.recv_timeout(Duration::from_secs(1)).unwrap();

        let (initial_template_worker_b, new_template_worker_b) =
            in_2.recv_timeout(Duration::from_secs(1)).unwrap();

        assert_eq!(
            initial_template_worker_a.prevhash,
            initial_template_worker_b.prevhash
        );

        assert_ne!(
            initial_template_worker_a.prevhash,
            new_template_worker_a.prevhash
        );

        assert_ne!(
            initial_template_worker_b.prevhash,
            new_template_worker_b.prevhash,
        );

        assert_eq!(
            new_template_worker_a.prevhash,
            new_template_worker_b.prevhash
        );

        assert!(new_template_worker_a.ntime >= initial_template_worker_a.ntime);
        assert!(new_template_worker_b.ntime >= initial_template_worker_b.ntime);
    });
}

#[test]
#[ignore]
fn aggregator_cache_concurrent_pool_burst() {
    let mut servers = Vec::new();
    for _ in 0..3 {
        let server = TestServer::spawn();
        fs::write(
            server.log_dir().join("pool/pool.status"),
            typical_status().to_string(),
        )
        .unwrap();

        servers.push(server);
    }

    let aggregator = Arc::new(TestServer::spawn_with_args(format!(
        "--nodes {} --nodes {} --nodes {} --ttl 1",
        servers[0].url(),
        servers[1].url(),
        servers[2].url(),
    )));

    aggregator.assert_response(
        "/aggregator/pool/pool.status",
        &(typical_status() + typical_status() + typical_status()).to_string(),
        None,
    );

    fs::write(
        servers[1].log_dir().join("pool/pool.status"),
        zero_status().to_string(),
    )
    .unwrap();

    const N: usize = 100;
    let start = Arc::new(Barrier::new(N + 1));
    let expected_old = (typical_status() + typical_status() + typical_status()).to_string();

    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let agg = aggregator.clone();
        let go = start.clone();
        let exp = expected_old.clone();
        handles.push(thread::spawn(move || {
            go.wait();
            agg.assert_response("/aggregator/pool/pool.status", &exp, None);
        }));
    }

    start.wait();

    for handle in handles {
        handle.join().unwrap();
    }

    thread::sleep(Duration::from_secs(1));

    let expected_new = (zero_status() + typical_status() + typical_status()).to_string();
    let start = Arc::new(Barrier::new(N + 1));
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let agg = aggregator.clone();
        let go = start.clone();
        let exp = expected_new.clone();
        handles.push(thread::spawn(move || {
            go.wait();
            agg.assert_response("/aggregator/pool/pool.status", &exp, None);
        }));
    }

    start.wait();

    for handles in handles {
        handles.join().unwrap();
    }
}

#[test]
#[ignore]
fn aggregator_cache_concurrent_user_burst() {
    let mut users = Vec::new();
    for i in 0..9 {
        let user = typical_user();
        let user_address = address(i);

        users.push((user_address.to_string(), user));
    }

    let mut servers = Vec::new();
    for (address, user) in users.iter().take(3) {
        let server = TestServer::spawn();
        fs::create_dir_all(server.log_dir().join("users")).unwrap();
        fs::write(
            server.log_dir().join(format!("users/{address}")),
            serde_json::to_string(&user).unwrap(),
        )
        .unwrap();
        servers.push(server);
    }

    let aggregator = Arc::new(TestServer::spawn_with_args(format!(
        "--nodes {} --nodes {} --nodes {} --ttl 1",
        servers[0].url(),
        servers[1].url(),
        servers[2].url(),
    )));

    let u0 = aggregator.get_json::<User>(format!("/aggregator/users/{}", users[0].0), None);
    pretty_assert_eq!(u0, typical_user());

    fs::write(
        servers[0].log_dir().join(format!("users/{}", users[0].0)),
        serde_json::to_string(&zero_user()).unwrap(),
    )
    .unwrap();

    const N: usize = 100;
    let start = Arc::new(Barrier::new(N + 1));
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let agg = aggregator.clone();
        let go = start.clone();
        let addr = users[0].0.clone();
        handles.push(thread::spawn(move || {
            go.wait();
            let got = agg.get_json::<User>(format!("/aggregator/users/{addr}"), None);
            pretty_assert_eq!(got, typical_user());
        }));
    }

    start.wait();

    for handle in handles {
        handle.join().unwrap();
    }

    thread::sleep(Duration::from_secs(1));

    let start = Arc::new(Barrier::new(N + 1));
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let agg = aggregator.clone();
        let go = start.clone();
        let addr = users[0].0.clone();
        handles.push(thread::spawn(move || {
            go.wait();
            let got = agg.get_json::<User>(format!("/aggregator/users/{addr}"), None);
            pretty_assert_eq!(got, zero_user());
        }));
    }

    start.wait();

    for handle in handles {
        handle.join().unwrap();
    }
}

#[tokio::test]
#[ignore]
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
        .run()
        .await
        .expect("Syncing between servers failed!");

    let pool = sqlx::PgPool::connect(&target_db_url).await.unwrap();

    let stored_shares: Vec<(i64, String)> = sqlx::query_as("SELECT id, origin FROM remote_shares")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(stored_shares.len() as u32, 300);

    let stored_block: Option<(i32, String)> = sqlx::query_as(
        "SELECT blockheight, blockhash FROM blocks ORDER BY blockheight ASC LIMIT 1",
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
