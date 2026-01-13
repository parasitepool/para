use super::*;

/// Compute expected enonce1_varlen based on ckpool's auto-selection logic.
fn expected_enonce1_varlen(upstream_nonce2_len: usize) -> usize {
    if upstream_nonce2_len > 7 {
        4
    } else if upstream_nonce2_len > 5 {
        2
    } else if upstream_nonce2_len > 3 {
        1
    } else {
        panic!("upstream_nonce2_len too small: {}", upstream_nonce2_len);
    }
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn proxy() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");
    let upstream = pool.stratum_endpoint();

    let proxy = TestProxy::spawn_with_args(
        &upstream,
        &signet_username().to_string(),
        "--start-diff 0.00001",
    );

    let status = proxy
        .get_status()
        .await
        .expect("Failed to get proxy status");

    assert_eq!(status.upstream, upstream, "Upstream URL should match");

    assert_eq!(
        status.upstream_username,
        signet_username(),
        "Username should match"
    );

    assert!(status.connected, "Proxy should be connected to upstream");

    let client = proxy.stratum_client();
    let mut events = client.connect().await.expect("Failed to connect to proxy");

    let (subscribe_result, _, _) = client
        .subscribe()
        .await
        .expect("Failed to subscribe through proxy");

    // Compute expected enonce1_varlen based on ckpool's auto-selection
    let enonce1_varlen = expected_enonce1_varlen(status.enonce2_size);

    // Downstream enonce1 = upstream enonce1 + enonce1_var
    assert_eq!(
        subscribe_result.enonce1.len(),
        status.enonce1.len() + enonce1_varlen,
        "Downstream enonce1 should be upstream enonce1 + {} bytes for enonce1_var",
        enonce1_varlen
    );

    // Downstream enonce2_size = upstream enonce2_size - enonce1_varlen
    assert_eq!(
        subscribe_result.enonce2_size,
        status.enonce2_size - enonce1_varlen,
        "Downstream enonce2_size should be upstream - {}",
        enonce1_varlen
    );

    client.authorize().await.expect("Failed to authorize");

    let (notify, difficulty) = wait_for_notify(&mut events).await;

    assert!(notify.clean_jobs, "Initial job should have clean_jobs=true");
    assert_eq!(
        difficulty,
        Difficulty::from(0.00001),
        "Difficulty should match configured start_diff"
    );

    let enonce2 = Extranonce::random(subscribe_result.enonce2_size);
    let (ntime, nonce) = solve_share(&notify, &subscribe_result.enonce1, &enonce2, difficulty);

    client
        .submit(notify.job_id, enonce2, ntime, nonce, None)
        .await
        .expect("Valid share should be accepted by proxy");

    let bad_enonce2 = Extranonce::random(subscribe_result.enonce2_size);
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
}

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
fn mine_through_proxy() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");

    let proxy = TestProxy::spawn_with_args(
        &pool.stratum_endpoint(),
        &signet_username().to_string(),
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

#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn proxy_enonce2_split_gives_unique_work() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");
    let upstream = pool.stratum_endpoint();

    let proxy = TestProxy::spawn_with_args(
        &upstream,
        &signet_username().to_string(),
        "--start-diff 0.00001",
    );

    let status = proxy
        .get_status()
        .await
        .expect("Failed to get proxy status");

    // Connect first miner
    let client1 = proxy.stratum_client();
    let mut events1 = client1.connect().await.expect("Failed to connect miner 1");

    let (subscribe1, _, _) = client1
        .subscribe()
        .await
        .expect("Failed to subscribe miner 1");

    // Connect second miner
    let client2 = proxy.stratum_client();
    let mut events2 = client2.connect().await.expect("Failed to connect miner 2");

    let (subscribe2, _, _) = client2
        .subscribe()
        .await
        .expect("Failed to subscribe miner 2");

    // Verify enonce1 values are different (each miner gets unique work)
    assert_ne!(
        subscribe1.enonce1, subscribe2.enonce1,
        "Miners should receive different enonce1 values for unique work"
    );

    // Compute expected enonce1_varlen based on ckpool's auto-selection
    let enonce1_varlen = expected_enonce1_varlen(status.enonce2_size);

    // Verify enonce2_size is reduced by enonce1_varlen
    assert_eq!(
        subscribe1.enonce2_size,
        status.enonce2_size - enonce1_varlen,
        "Downstream enonce2_size should be upstream - {}",
        enonce1_varlen
    );
    assert_eq!(
        subscribe2.enonce2_size,
        status.enonce2_size - enonce1_varlen,
        "Both miners should have same enonce2_size"
    );

    // Verify the enonce1 has the expected structure:
    // downstream_enonce1 = upstream_enonce1 (const) + enonce1_var
    assert_eq!(
        subscribe1.enonce1.len(),
        status.enonce1.len() + enonce1_varlen,
        "Downstream enonce1 should be upstream enonce1 + {} bytes for enonce1_var",
        enonce1_varlen
    );

    // Both miners should be able to authorize and submit valid shares
    client1.authorize().await.expect("Miner 1 authorize failed");
    client2.authorize().await.expect("Miner 2 authorize failed");

    let (notify1, difficulty1) = wait_for_notify(&mut events1).await;
    let (notify2, difficulty2) = wait_for_notify(&mut events2).await;

    // Solve and submit shares for both miners
    let enonce2_1 = Extranonce::random(subscribe1.enonce2_size);
    let (ntime1, nonce1) = solve_share(&notify1, &subscribe1.enonce1, &enonce2_1, difficulty1);
    client1
        .submit(notify1.job_id, enonce2_1, ntime1, nonce1, None)
        .await
        .expect("Miner 1 share should be accepted");

    let enonce2_2 = Extranonce::random(subscribe2.enonce2_size);
    let (ntime2, nonce2) = solve_share(&notify2, &subscribe2.enonce1, &enonce2_2, difficulty2);
    client2
        .submit(notify2.job_id, enonce2_2, ntime2, nonce2, None)
        .await
        .expect("Miner 2 share should be accepted");
}
