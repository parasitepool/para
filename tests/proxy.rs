use super::*;

#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn proxy() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");
    let upstream = pool.stratum_endpoint();
    let username = signet_username();

    let proxy = TestProxy::spawn_with_args(
        &upstream,
        &username.to_string(),
        pool.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let status = proxy.get_status().await.unwrap();

    assert_eq!(status.endpoint, proxy.stratum_endpoint());

    let system_status = proxy.get_system_status().await.unwrap();
    assert!(system_status.cpu_usage_percent >= 0.0 && system_status.cpu_usage_percent <= 100.0);
    assert!(
        system_status.memory_usage_percent >= 0.0 && system_status.memory_usage_percent <= 100.0
    );
    assert!(system_status.disk_usage_percent >= 0.0 && system_status.disk_usage_percent <= 100.0);
    assert!(system_status.uptime > 0);

    let bitcoin_status = proxy.get_bitcoin_status().await.unwrap();
    assert!(bitcoin_status.difficulty > 0.0);

    assert_eq!(
        status.upstream_endpoint, upstream,
        "Upstream URL should match"
    );

    assert_eq!(status.upstream_username, username, "Username should match");

    assert!(
        status.upstream_connected,
        "Proxy should be connected to upstream"
    );

    assert_eq!(status.users, 0);
    assert_eq!(status.workers, 0);
    assert_eq!(status.connections, 0);
    assert_eq!(status.accepted, 0);
    assert_eq!(status.rejected, 0);
    assert_eq!(status.upstream_accepted, 0);
    assert_eq!(status.upstream_rejected, 0);
    assert!((status.upstream_difficulty - 0.00001).abs() < 1e-9);
    assert!(status.best_ever.is_none());
    assert!(status.last_share.is_none());

    let client = proxy.stratum_client();
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();

    let upstream_enonce1 = status.upstream_enonce1.as_bytes();
    let extended_enonce1 = subscribe.enonce1.as_bytes();
    assert_eq!(
        &extended_enonce1[..upstream_enonce1.len()],
        upstream_enonce1,
        "Extended enonce1 should start with upstream enonce1"
    );
    assert_eq!(
        extended_enonce1.len(),
        upstream_enonce1.len() + ENONCE1_EXTENSION_SIZE,
        "Extended enonce1 should be upstream + extension bytes"
    );

    assert_eq!(
        subscribe.enonce2_size,
        status.upstream_enonce2_size - ENONCE1_EXTENSION_SIZE,
        "Miner enonce2_size should be upstream minus extension"
    );

    client.authorize().await.unwrap();

    let (notify, difficulty) = wait_for_notify(&mut events).await;

    assert!(notify.clean_jobs, "Initial job should have clean_jobs=true");
    assert_eq!(
        difficulty,
        Difficulty::from(0.00001),
        "Difficulty should match configured start_diff"
    );

    let enonce2 = Extranonce::random(subscribe.enonce2_size);
    let (ntime, nonce) = solve_share(&notify, &subscribe.enonce1, &enonce2, difficulty);

    client
        .submit(notify.job_id, enonce2, ntime, nonce, None)
        .await
        .unwrap();

    let user_address = username
        .parse_address()
        .unwrap()
        .assume_checked()
        .to_string();

    let status = proxy.get_status().await.unwrap();
    assert_eq!(status.users, 1);
    assert_eq!(status.workers, 1);
    assert_eq!(status.connections, 1);
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 0);
    assert_eq!(status.upstream_accepted, 1);
    assert_eq!(status.upstream_rejected, 0);
    assert!(status.best_ever.is_some());
    assert!(status.last_share.is_some());

    let user = proxy.get_user(&user_address).await.unwrap();
    assert_eq!(user.address, user_address);
    assert_eq!(user.accepted, 1);
    assert_eq!(user.rejected, 0);
    assert!(user.best_ever.is_some());
    assert_eq!(user.workers.len(), 1);
    assert_eq!(user.workers[0].accepted, 1);
    assert_eq!(user.workers[0].rejected, 0);
    assert!(user.workers[0].best_ever.is_some());

    let bad_enonce2 = Extranonce::random(subscribe.enonce2_size);
    let result = client
        .submit(
            notify.job_id,
            bad_enonce2,
            notify.ntime,
            Nonce::from(0),
            None,
        )
        .await;

    assert_stratum_error(result, StratumError::AboveTarget);

    let status = proxy.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 1);
    assert_eq!(status.upstream_accepted, 1);
    assert_eq!(status.upstream_rejected, 0);

    assert_stratum_error(
        client
            .submit(
                notify.job_id,
                Extranonce::random(subscribe.enonce2_size - 1),
                notify.ntime,
                Nonce::from(0),
                None,
            )
            .await,
        StratumError::InvalidNonce2Length,
    );

    let status = proxy.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 2);

    assert_stratum_error(
        client
            .submit(
                notify.job_id,
                Extranonce::random(subscribe.enonce2_size + 1),
                notify.ntime,
                Nonce::from(0),
                None,
            )
            .await,
        StratumError::InvalidNonce2Length,
    );

    let status = proxy.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 3);

    let user = proxy.get_user(&user_address).await.unwrap();
    assert_eq!(user.accepted, 1);
    assert_eq!(user.rejected, 3);
    assert_eq!(user.workers[0].accepted, 1);
    assert_eq!(user.workers[0].rejected, 3);

    client.disconnect().await;
    drop(events);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client2 = proxy.stratum_client();
    let mut events2 = client2.connect().await.unwrap();

    let (subscribe2, _, _) = client2
        .subscribe_with_enonce1(Some(subscribe.enonce1.clone()))
        .await
        .unwrap();

    assert_eq!(
        subscribe2.enonce1, subscribe.enonce1,
        "Session resume should return same extended enonce1"
    );
    assert_eq!(
        subscribe2.enonce2_size, subscribe.enonce2_size,
        "Session resume should return same enonce2_size"
    );

    client2.authorize().await.unwrap();

    let (notify2, _) = wait_for_notify(&mut events2).await;

    let enonce2_resumed = Extranonce::random(subscribe2.enonce2_size);
    let (ntime2, nonce2) = solve_share(&notify2, &subscribe2.enonce1, &enonce2_resumed, difficulty);

    client2
        .submit(notify2.job_id, enonce2_resumed, ntime2, nonce2, None)
        .await
        .unwrap();

    let status = proxy.get_status().await.unwrap();
    assert_eq!(status.accepted, 2);
    assert_eq!(status.rejected, 3);
    assert_eq!(status.upstream_accepted, 2);
    assert_eq!(status.upstream_rejected, 0);

    let user = proxy.get_user(&user_address).await.unwrap();
    assert_eq!(user.accepted, 2);
    assert_eq!(user.rejected, 3);
    assert_eq!(user.workers[0].accepted, 2);
    assert_eq!(user.workers[0].rejected, 3);
}

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
fn mine_through_proxy() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");

    let proxy = TestProxy::spawn_with_args(
        &pool.stratum_endpoint(),
        &signet_username().to_string(),
        pool.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let miner = CommandBuilder::new(format!(
        "miner {} --mode share-found --username {} --cpu-cores 1",
        proxy.stratum_endpoint(),
        signet_username()
    ))
    .spawn();

    let stdout = miner.wait_with_output().unwrap();

    assert_eq!(
        stdout.status.code(),
        Some(0),
        "Miner should exit successfully"
    );

    let output =
        serde_json::from_str::<Vec<Share>>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    assert_eq!(output.len(), 1, "Should find exactly one share");
}

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
fn proxy_rejects_incompatible_upstream_enonce2_size() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001 --enonce2-size 2");

    let stderr = TestProxy::spawn_expect_failure(
        &pool.stratum_endpoint(),
        &signet_username().to_string(),
        pool.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    assert!(
        stderr.contains("upstream extranonce configuration incompatible with proxy mode")
            || stderr.contains("too small to carve out")
            || stderr.contains("miner enonce2 space")
            || stderr.contains("below minimum"),
        "Expected error about incompatible enonce configuration, got: {}",
        stderr
    );
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn proxy_with_non_default_enonce_sizes() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001 --enonce1-size 6 --enonce2-size 4");
    let upstream = pool.stratum_endpoint();

    let proxy = TestProxy::spawn_with_args(
        &upstream,
        &signet_username().to_string(),
        pool.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let status = proxy.get_status().await.unwrap();

    assert_eq!(
        status.upstream_enonce1.len(),
        6,
        "Upstream enonce1 should be 6 bytes"
    );
    assert_eq!(
        status.upstream_enonce2_size, 4,
        "Upstream enonce2 should be 4 bytes"
    );

    let client = proxy.stratum_client();
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();

    assert_eq!(
        subscribe.enonce1.len(),
        8,
        "Extended enonce1 should be 8 bytes (6 upstream + 2 extension)"
    );
    assert_eq!(
        subscribe.enonce2_size, 2,
        "Miner enonce2 should be 2 bytes (4 upstream - 2 extension)"
    );

    client.authorize().await.unwrap();

    let (notify, difficulty) = wait_for_notify(&mut events).await;

    let enonce2 = Extranonce::random(subscribe.enonce2_size);
    let (ntime, nonce) = solve_share(&notify, &subscribe.enonce1, &enonce2, difficulty);

    client
        .submit(notify.job_id, enonce2, ntime, nonce, None)
        .await
        .unwrap();
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn proxy_allows_version_rolling() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");

    let proxy = TestProxy::spawn_with_args(
        &pool.stratum_endpoint(),
        &signet_username().to_string(),
        pool.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    assert_eq!(
        proxy.get_status().await.unwrap().upstream_version_mask,
        Some(Version::from_str("1fffe000").unwrap()),
        "Upstream version mask should match pool's configured mask"
    );

    let miner_with_version_rolling = CommandBuilder::new(format!(
        "miner {} --mode share-found --username {} --cpu-cores 1",
        proxy.stratum_endpoint(),
        signet_username()
    ))
    .spawn();

    let miner = CommandBuilder::new(format!(
        "miner {} --mode share-found --username {} --cpu-cores 1 --disable-version-rolling",
        proxy.stratum_endpoint(),
        signet_username()
    ))
    .spawn();

    let output_with_version_rolling = miner_with_version_rolling.wait_with_output().unwrap();
    let output = miner.wait_with_output().unwrap();

    assert_eq!(output_with_version_rolling.status.code(), Some(0));
    assert_eq!(output.status.code(), Some(0));

    let shares_with_version_rolling: Vec<Share> = serde_json::from_str(&String::from_utf8_lossy(
        &output_with_version_rolling.stdout,
    ))
    .unwrap();

    let shares: Vec<Share> =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();

    assert_eq!(shares_with_version_rolling.len(), 1);
    assert_eq!(shares.len(), 1);

    assert!(
        shares_with_version_rolling[0].version_bits.is_some(),
        "Miner with version rolling should have version_bits set"
    );

    assert!(
        shares[0].version_bits.is_none(),
        "Miner without version rolling should not have version_bits set"
    );
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(120000)]
#[ignore]
async fn proxy_relays_job_updates_and_new_blocks() {
    let pool = TestPool::spawn_with_args("--start-diff 0.000001 --update-interval 1");

    let proxy = TestProxy::spawn_with_args(
        &pool.stratum_endpoint(),
        signet_username().as_str(),
        pool.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let client = proxy.stratum_client();
    let mut events = client.connect().await.unwrap();

    client.subscribe().await.unwrap();
    client.authorize().await.unwrap();

    let (notify, _) = wait_for_notify(&mut events).await;

    assert!(notify.clean_jobs, "Initial job should be clean_jobs=true");

    let updated = wait_for_job_update(&mut events, notify.job_id).await;

    assert!(!updated.clean_jobs);

    pool.mine_block().await;

    let new_block = wait_for_new_block(&mut events, updated.job_id).await;

    assert!(new_block.clean_jobs);
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(120000)]
async fn proxy_exits_on_upstream_disconnect() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");

    let proxy = TestProxy::spawn_with_args(
        &pool.stratum_endpoint(),
        signet_username().as_str(),
        pool.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let client = proxy.stratum_client();
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();
    client.authorize().await.unwrap();
    let (notify, difficulty) = wait_for_notify(&mut events).await;

    let status = proxy.get_status().await.unwrap();
    assert!(status.upstream_connected);

    drop(pool);

    let enonce2 = Extranonce::random(subscribe.enonce2_size);
    let (ntime, nonce) = solve_share(&notify, &subscribe.enonce1, &enonce2, difficulty);

    assert!(
        client
            .submit(notify.job_id, enonce2, ntime, nonce, None)
            .await
            .is_err(),
        "Miner should be disconnected after upstream loss"
    );
}
