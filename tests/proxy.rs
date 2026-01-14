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

    let upstream_enonce1 = status.enonce1.as_bytes();
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
        status.enonce2_size - ENONCE1_EXTENSION_SIZE,
        "Miner enonce2_size should be upstream minus extension"
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

    client.disconnect().await;
    drop(events);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let client2 = proxy.stratum_client();
    let mut events2 = client2.connect().await.expect("Failed to reconnect");

    let (subscribe2, _, _) = client2
        .subscribe_with_enonce1(Some(subscribe.enonce1.clone()))
        .await
        .expect("Failed to subscribe with enonce1");

    assert_eq!(
        subscribe2.enonce1, subscribe.enonce1,
        "Session resume should return same extended enonce1"
    );
    assert_eq!(
        subscribe2.enonce2_size, subscribe.enonce2_size,
        "Session resume should return same enonce2_size"
    );

    client2.authorize().await.expect("Failed to authorize");

    let (notify2, _) = wait_for_notify(&mut events2).await;

    let enonce2_resumed = Extranonce::random(subscribe2.enonce2_size);
    let (ntime2, nonce2) = solve_share(&notify2, &subscribe2.enonce1, &enonce2_resumed, difficulty);

    client2
        .submit(notify2.job_id, enonce2_resumed, ntime2, nonce2, None)
        .await
        .expect("Share with resumed session should be accepted");
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
async fn proxy_with_non_default_enonce_sizes() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001 --enonce1-size 6 --enonce2-size 4");
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

    assert_eq!(
        status.enonce1.len(),
        6,
        "Upstream enonce1 should be 6 bytes"
    );
    assert_eq!(status.enonce2_size, 4, "Upstream enonce2 should be 4 bytes");

    let client = proxy.stratum_client();
    let mut events = client.connect().await.expect("Failed to connect to proxy");

    let (subscribe, _, _) = client
        .subscribe()
        .await
        .expect("Failed to subscribe through proxy");

    assert_eq!(
        subscribe.enonce1.len(),
        8,
        "Extended enonce1 should be 8 bytes (6 upstream + 2 extension)"
    );
    assert_eq!(
        subscribe.enonce2_size, 2,
        "Miner enonce2 should be 2 bytes (4 upstream - 2 extension)"
    );

    client.authorize().await.expect("Failed to authorize");

    let (notify, difficulty) = wait_for_notify(&mut events).await;

    let enonce2 = Extranonce::random(subscribe.enonce2_size);
    let (ntime, nonce) = solve_share(&notify, &subscribe.enonce1, &enonce2, difficulty);

    client
        .submit(notify.job_id, enonce2, ntime, nonce, None)
        .await
        .expect("Valid share should be accepted - job must use correct enonce2_size");
}
