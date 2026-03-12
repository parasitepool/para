use super::*;

#[tokio::test]
#[serial(bitcoind)]
#[timeout(120000)]
async fn router_round_robin() {
    let pool_a = TestPool::spawn_with_args(global_bitcoind(), "--start-diff 0.00001");
    let pool_b = TestPool::spawn_with_args(global_bitcoind(), "--start-diff 0.00001");

    let username_a = "tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.foo";
    let username_b = "tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.bar";

    let mut router = TestRouter::spawn(
        &[
            (username_a, &pool_a.stratum_endpoint()),
            (username_b, &pool_b.stratum_endpoint()),
        ],
        pool_a.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let status = router.get_status().await.unwrap();
    assert_eq!(status.slots.len(), 2);
    assert_eq!(status.session_count, 0);

    let mut miners = Vec::new();

    for _ in 0..3 {
        miners.push(
            CommandBuilder::new(format!(
                "miner {} --mode continuous --username {} --cpu-cores 1",
                router.stratum_endpoint(),
                signet_username()
            ))
            .spawn(),
        );
    }

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.session_count >= 3
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for 3 sessions");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.slots.len(), 2);
    assert_eq!(status.session_count, 3);

    drop(pool_a);

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.slots.len() == 1
                && status.session_count >= 3
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for miners to reconnect to remaining upstream");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.slots.len(), 1);
    assert_eq!(status.session_count, 3);

    drop(pool_b);

    timeout(Duration::from_secs(30), async {
        loop {
            if router.try_wait().unwrap().is_some() {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for router to exit");

    for mut miner in miners {
        miner.kill().unwrap();
        miner.wait().unwrap();
    }
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(120000)]
async fn router_rejects_incompatible_resumed_enonce1() {
    let pool_a = TestPool::spawn_with_args(
        global_bitcoind(),
        "--start-diff 0.00001 --enonce1-size 4 --enonce2-size 8",
    );
    let pool_b = TestPool::spawn_with_args(
        global_bitcoind(),
        "--start-diff 0.00001 --enonce1-size 6 --enonce2-size 6",
    );

    let router = TestRouter::spawn(
        &[
            (signet_username().as_str(), &pool_a.stratum_endpoint()),
            (signet_username().as_str(), &pool_b.stratum_endpoint()),
        ],
        pool_a.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let client_a = stratum::client::Client::new(
        router.stratum_endpoint(),
        signet_username(),
        None,
        USER_AGENT.into(),
        Duration::from_secs(5),
    );
    let mut events_a = client_a.connect().await.unwrap();

    let (subscribe_a, _, _) = client_a.subscribe().await.unwrap();
    assert_eq!(subscribe_a.enonce1.len(), 4 + ENONCE1_EXTENSION_SIZE);
    assert_eq!(subscribe_a.enonce2_size, 8 - ENONCE1_EXTENSION_SIZE);

    client_a.authorize().await.unwrap();

    let (notify_a, difficulty_a) = wait_for_notify(&mut events_a).await;
    let enonce2_a = Extranonce::random(subscribe_a.enonce2_size);
    let (ntime_a, nonce_a) = solve_share(&notify_a, &subscribe_a.enonce1, &enonce2_a, difficulty_a);

    client_a
        .submit(notify_a.job_id, enonce2_a, ntime_a, nonce_a, None)
        .await
        .unwrap();

    client_a.disconnect().await;
    drop(events_a);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let client_b = stratum::client::Client::new(
        router.stratum_endpoint(),
        signet_username(),
        None,
        USER_AGENT.into(),
        Duration::from_secs(5),
    );
    let mut events_b = client_b.connect().await.unwrap();

    let (subscribe_b, _, _) = client_b
        .subscribe_with_enonce1(Some(subscribe_a.enonce1.clone()))
        .await
        .unwrap();

    assert_ne!(subscribe_b.enonce1, subscribe_a.enonce1);
    assert_eq!(subscribe_b.enonce1.len(), 6 + ENONCE1_EXTENSION_SIZE);
    assert_eq!(subscribe_b.enonce2_size, 6 - ENONCE1_EXTENSION_SIZE);

    client_b.authorize().await.unwrap();

    let (notify_b, difficulty_b) = wait_for_notify(&mut events_b).await;
    let enonce2_b = Extranonce::random(subscribe_b.enonce2_size);
    let (ntime_b, nonce_b) = solve_share(&notify_b, &subscribe_b.enonce1, &enonce2_b, difficulty_b);

    client_b
        .submit(notify_b.job_id, enonce2_b, ntime_b, nonce_b, None)
        .await
        .unwrap();
}
