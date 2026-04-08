use super::*;

fn generate_descriptor() -> String {
    CommandBuilder::new("wallet --chain regtest generate")
        .run_and_deserialize_output::<para::subcommand::wallet::generate::Output>()
        .descriptor
}

async fn fund_wallet(bitcoind: &Bitcoind, descriptor: &str) -> String {
    let address = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {descriptor} \
         receive",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<para::subcommand::wallet::receive::Output>()
    .address
    .assume_checked()
    .to_string();

    generate_to_address(bitcoind, 101, &address).await;

    address
}

async fn pay_address(bitcoind: &Bitcoind, funding_descriptor: &str, address: &str, amount: u64) {
    CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {funding_descriptor} \
         send --fee-rate 1 --address {address} --amount {amount}",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<para::subcommand::wallet::send::Output>();

    generate_to_address(bitcoind, 1, address).await;
}

async fn add_order(
    router: &TestRouter,
    target: &str,
    amount: u64,
    target_work: Option<HashDays>,
) -> (u32, String) {
    let response = router
        .add_order(&api::OrderRequest {
            target: target.parse().unwrap(),
            target_work,
            amount: Amount::from_sat(amount),
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = response.json().await.unwrap();

    (
        body["id"].as_u64().unwrap() as u32,
        body["address"].as_str().unwrap().to_string(),
    )
}

async fn add_and_activate_order(
    router: &TestRouter,
    wallet_bitcoind: &Bitcoind,
    funding_descriptor: &str,
    target: &str,
    amount: u64,
    target_work: Option<HashDays>,
) -> u32 {
    let (id, address) = add_order(router, target, amount, target_work).await;

    pay_address(wallet_bitcoind, funding_descriptor, &address, amount).await;

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status
                    .orders
                    .iter()
                    .any(|o| o.id == id && o.status == OrderStatus::Active)
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for order to become active");

    id
}

#[tokio::test]
#[timeout(120000)]
async fn router_round_robin() {
    let pool_bitcoind = bitcoind();
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let pool_username = signet_username();
    let miner_address = fund_wallet(&wallet_bitcoind, &generate_descriptor()).await;
    let miner_username = format!("{miner_address}.miner");

    let pool_a = TestPool::spawn_with_args(&pool_bitcoind, "--start-diff 0.00001");
    let pool_b = TestPool::spawn_with_args(&pool_bitcoind, "--start-diff 0.00001");

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    let status = router.get_status().await.unwrap();
    assert_eq!(status.orders.len(), 0);

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{pool_username}@{}", pool_a.stratum_endpoint()),
        1000,
        None,
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{pool_username}@{}", pool_b.stratum_endpoint()),
        1000,
        None,
    )
    .await;

    let status = router.get_status().await.unwrap();
    assert_eq!(status.orders.len(), 2);
    assert_eq!(status.active_orders, 2);

    let mut miners = Vec::new();

    for _ in 0..3 {
        miners.push(
            CommandBuilder::new(format!(
                "miner {} --mode continuous --username {} --cpu-cores 1",
                router.stratum_endpoint(),
                miner_username
            ))
            .spawn(),
        );
    }

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.active_orders == 2
                && status.session_count >= 3
            {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Timeout waiting for 2 slots and 3 sessions");

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
            sleep(Duration::from_millis(50)).await;
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
            sleep(Duration::from_millis(50)).await;
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
#[timeout(120000)]
async fn orders() {
    let pool_bitcoind = bitcoind();
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let username = signet_username();

    let pool_a = TestPool::spawn_with_args(&pool_bitcoind, "--start-diff 0.00001");
    let pool_b = TestPool::spawn_with_args(&pool_bitcoind, "--start-diff 0.00001");

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    let status = router.get_status().await.unwrap();
    assert_eq!(status.orders.len(), 0);

    let response = router
        .add_order(&api::OrderRequest {
            target_work: None,
            target: format!("{username}@127.0.0.1:1").parse().unwrap(),
            amount: Amount::from_sat(1000),
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{username}@{}", pool_a.stratum_endpoint()),
        1000,
        None,
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{username}@{}", pool_b.stratum_endpoint()),
        1000,
        None,
    )
    .await;

    let status = router.get_status().await.unwrap();
    assert_eq!(status.active_orders, 2);

    let response = router.remove_order(9999).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let active_id = status
        .orders
        .iter()
        .find(|o| o.status == OrderStatus::Active)
        .unwrap()
        .id;
    let response = router.remove_order(active_id).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let status = router.get_status().await.unwrap();
    assert_eq!(status.orders.len(), 3);
    assert_eq!(status.active_orders, 1);
}

#[tokio::test]
#[timeout(120000)]
async fn cancelled_order_stays_cancelled_during_activation() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let stalled_port = allocate_port();
    let stalled_server = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", stalled_port))
            .await
            .unwrap();
        let _ = listener.accept().await;
        sleep(Duration::from_secs(10)).await;
    });

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--tick-interval 1 --timeout 2",
    );

    let (id, address) = add_order(
        &router,
        &format!("{}@127.0.0.1:{stalled_port}", signet_username()),
        1000,
        None,
    )
    .await;

    pay_address(&wallet_bitcoind, &funding_descriptor, &address, 1000).await;
    sleep(Duration::from_millis(1200)).await;

    let response = router.remove_order(id).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    sleep(Duration::from_secs(3)).await;

    let status = router.get_status().await.unwrap();
    let order = status.orders.iter().find(|order| order.id == id).unwrap();

    assert_eq!(order.status, OrderStatus::Cancelled);
    assert_eq!(status.active_orders, 0);

    stalled_server.abort();
}

#[tokio::test]
#[timeout(120000)]
async fn order_disconnected_on_upstream_disconnect() {
    let pool_bitcoind = bitcoind();
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let username = signet_username();

    let pool_a = TestPool::spawn_with_args(&pool_bitcoind, "--start-diff 0.00001");
    let pool_b = TestPool::spawn_with_args(&pool_bitcoind, "--start-diff 0.00001");

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{username}@{}", pool_a.stratum_endpoint()),
        1000,
        None,
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{username}@{}", pool_b.stratum_endpoint()),
        1000,
        None,
    )
    .await;

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
#[timeout(120000)]
async fn order_fulfilled_on_target_work_reached() {
    let pool_bitcoind = bitcoind();
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let pool_username = signet_username();
    let miner_address = fund_wallet(&wallet_bitcoind, &generate_descriptor()).await;
    let miner_username = format!("{miner_address}.miner");

    let pool = TestPool::spawn_with_args(&pool_bitcoind, "--start-diff 0.00001");

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{pool_username}@{}", pool.stratum_endpoint()),
        1000,
        Some(HashDays(1e-10)),
    )
    .await;

    let status = router.get_status().await.unwrap();
    assert_eq!(status.active_orders, 1);

    let mut miner = CommandBuilder::new(format!(
        "miner {} --mode continuous --username {} --cpu-cores 1",
        router.stratum_endpoint(),
        miner_username
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
    assert!(fulfilled.upstream.as_ref().unwrap().hash_days >= HashDays(1e-10));

    miner.kill().unwrap();
    miner.wait().unwrap();
}

#[tokio::test]
#[timeout(120000)]
async fn router_rejects_incompatible_resumed_enonce1() {
    let pool_bitcoind = bitcoind();
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let upstream_username = signet_username();
    let miner_address = fund_wallet(&wallet_bitcoind, &generate_descriptor()).await;
    let miner_username = Username::new(format!("{miner_address}.miner"));

    let pool_a = TestPool::spawn_with_args(
        &pool_bitcoind,
        "--start-diff 0.00001 --enonce1-size 4 --enonce2-size 8",
    );
    let pool_b = TestPool::spawn_with_args(
        &pool_bitcoind,
        "--start-diff 0.00001 --enonce1-size 6 --enonce2-size 6",
    );

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{}@{}", upstream_username, pool_a.stratum_endpoint()),
        1000,
        None,
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{}@{}", upstream_username, pool_b.stratum_endpoint()),
        1000,
        None,
    )
    .await;

    let client_a = stratum::client::Client::new(
        router.stratum_endpoint(),
        miner_username.clone(),
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
        miner_username,
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
#[timeout(120000)]
async fn list_orders_by_address() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    let username_a = signet_username();
    let username_b = Username::new("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.bar");

    add_order(&router, &format!("{username_a}@foo:3333"), 1000, None).await;
    add_order(&router, &format!("{username_a}@foo:4444"), 1000, None).await;
    add_order(&router, &format!("{username_b}@foo:5555"), 1000, None).await;

    let all = router.list_orders(None).await.unwrap();
    assert_eq!(all.len(), 3);

    let filtered_a = router
        .list_orders(Some(username_a.address_str().unwrap()))
        .await
        .unwrap();
    assert_eq!(filtered_a.len(), 2);

    let filtered_b = router
        .list_orders(Some(username_b.address_str().unwrap()))
        .await
        .unwrap();
    assert_eq!(filtered_b.len(), 1);

    let filtered_none = router.list_orders(Some("tb1qnonexistent")).await.unwrap();
    assert_eq!(filtered_none.len(), 0);
}
