use super::*;

#[tokio::test]
#[serial(bitcoind)]
#[timeout(120000)]
async fn router_round_robin() {
    let pool_a = TestPool::spawn_with_args(bitcoind(), "--start-diff 0.00001");
    let pool_b = TestPool::spawn_with_args(bitcoind(), "--start-diff 0.00001");

    let username_a = "tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.foo";
    let username_b = "tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.bar";

    let router = TestRouter::spawn(
        &[
            (username_a, &pool_a.stratum_endpoint()),
            (username_b, &pool_b.stratum_endpoint()),
        ],
        pool_a.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let status = router.get_status().await.unwrap();
    assert_eq!(status.orders.len(), 2);
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
    assert_eq!(status.active_orders, 2);
    assert_eq!(status.session_count, 3);

    drop(pool_a);

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.active_orders == 1
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
    assert_eq!(status.active_orders, 1);
    assert_eq!(status.orders.len(), 2);
    assert_eq!(status.session_count, 3);

    drop(pool_b);

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.active_orders == 0
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for all upstreams to disconnect");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.active_orders, 0);
    assert_eq!(status.orders.len(), 2);

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
        bitcoind(),
        "--start-diff 0.00001 --enonce1-size 4 --enonce2-size 8",
    );
    let pool_b = TestPool::spawn_with_args(
        bitcoind(),
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

#[tokio::test]
#[serial(bitcoind)]
#[timeout(120000)]
async fn orders() {
    let pool_a = TestPool::spawn_with_args(bitcoind(), "--start-diff 0.00001");
    let pool_b = TestPool::spawn_with_args(bitcoind(), "--start-diff 0.00001");

    let username = "tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.foo";

    let router = TestRouter::spawn(
        &[(username, &pool_a.stratum_endpoint())],
        pool_a.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let status = router.get_status().await.unwrap();
    assert_eq!(status.orders.len(), 1);

    let response = router
        .add_order(&api::OrderRequest {
            target_work: None,
            target: format!("{username}@127.0.0.1:1").parse().unwrap(),
        })
        .await
        .unwrap();

    assert_ne!(response.status(), StatusCode::OK);

    let response = router
        .add_order(&api::OrderRequest {
            target_work: None,
            target: format!("{username}@{}", pool_b.stratum_endpoint())
                .parse()
                .unwrap(),
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let status = router.get_status().await.unwrap();
    assert_eq!(status.active_orders, 2);

    let response = router.remove_order(9999).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let id = status.orders[0].id;
    let response = router.remove_order(id).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let status = router.get_status().await.unwrap();
    assert_eq!(status.orders.len(), 2);
    assert_eq!(status.active_orders, 1);
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(120000)]
async fn order_disconnected_on_upstream_disconnect() {
    let pool_a = TestPool::spawn_with_args(bitcoind(), "--start-diff 0.00001");
    let pool_b = TestPool::spawn_with_args(bitcoind(), "--start-diff 0.00001");

    let username = "tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.foo";

    let router = TestRouter::spawn(
        &[(username, &pool_a.stratum_endpoint())],
        pool_a.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let status = router.get_status().await.unwrap();
    assert_eq!(status.orders.len(), 1);

    let response = router
        .add_order(&api::OrderRequest {
            target_work: None,
            target: format!("{username}@{}", pool_b.stratum_endpoint())
                .parse()
                .unwrap(),
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let status = router.get_status().await.unwrap();
    assert_eq!(status.active_orders, 2);

    drop(pool_b);

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.active_orders == 1
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for order to be marked disconnected after upstream disconnect");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.orders.len(), 2);
    assert_eq!(status.active_orders, 1);
    assert_eq!(
        status
            .orders
            .iter()
            .filter(|o| o.status == OrderStatus::Disconnected)
            .count(),
        1
    );
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(120000)]
async fn order_fulfilled_on_target_work_reached() {
    let pool = TestPool::spawn_with_args(bitcoind(), "--start-diff 0.00001");

    let username = "tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.foo";

    let router = TestRouter::spawn(
        &[],
        pool.bitcoind_rpc_port(),
        "--start-diff 0.00001 --tick-interval 1",
    );

    let response = router
        .add_order(&api::OrderRequest {
            target_work: Some(HashDays(1e-10)),
            target: format!("{username}@{}", pool.stratum_endpoint())
                .parse()
                .unwrap(),
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let status = router.get_status().await.unwrap();
    assert_eq!(status.active_orders, 1);

    let mut miner = CommandBuilder::new(format!(
        "miner {} --mode continuous --username {} --cpu-cores 1",
        router.stratum_endpoint(),
        signet_username()
    ))
    .spawn();

    timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status
                    .orders
                    .iter()
                    .any(|o| o.status == OrderStatus::Fulfilled)
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for order to be fulfilled");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.active_orders, 0);

    let fulfilled = status
        .orders
        .iter()
        .find(|o| o.status == OrderStatus::Fulfilled)
        .unwrap();

    assert_eq!(fulfilled.target_work, Some(HashDays(1e-10)));
    assert!(fulfilled.upstream.hash_days >= HashDays(1e-10));

    miner.kill().unwrap();
    miner.wait().unwrap();
}
