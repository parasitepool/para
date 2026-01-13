use super::*;

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

    let (subscribe, _, _) = client
        .subscribe()
        .await
        .expect("Failed to subscribe through proxy");

    assert_eq!(
        subscribe.enonce1, status.enonce1,
        "Proxy should relay upstream's enonce1"
    );

    assert_eq!(
        subscribe.enonce2_size, status.enonce2_size,
        "Proxy should relay upstream's enonce2_size"
    );

    client.authorize().await.expect("Failed to authorize");

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
        .expect("Valid share should be accepted by proxy");

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
