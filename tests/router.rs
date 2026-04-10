use super::*;

fn generate_descriptor() -> String {
    CommandBuilder::new("wallet --chain regtest generate")
        .run_and_deserialize_output::<para::subcommand::wallet::generate::Output>()
        .descriptor
}

fn dust_limit(descriptor: &str) -> u64 {
    CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port 1 \
         --bitcoin-rpc-username foo \
         --bitcoin-rpc-password bar \
         --descriptor {descriptor} \
         receive"
    ))
    .run_and_deserialize_output::<para::subcommand::wallet::receive::Output>()
    .address
    .assume_checked()
    .script_pubkey()
    .minimal_non_dust()
    .to_sat()
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

async fn add_order(router: &TestRouter, target: &str, target_work: HashDays) -> (u32, String, u64) {
    let response = router
        .add_order(&api::OrderRequest {
            target: target.parse().unwrap(),
            target_work,
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let location = response
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let body: api::AddOrderResponse = response.json().await.unwrap();

    assert_eq!(location, format!("/api/router/order/{}", body.id));

    (
        body.id,
        body.address.assume_checked().to_string(),
        body.amount,
    )
}

async fn add_and_activate_order(
    router: &TestRouter,
    wallet_bitcoind: &Bitcoind,
    funding_descriptor: &str,
    target: &str,
    target_work: HashDays,
) -> u32 {
    let (id, address, amount) = add_order(router, target, target_work).await;

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
async fn add_order_without_target_work_rejected() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --hashprice 1000",
    );

    let response = reqwest::Client::new()
        .post(format!("{}/api/router/order", router.api_endpoint()))
        .json(&json!({
            "target": {
                "endpoint": "bar:3333",
                "username": "foo"
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
#[timeout(120000)]
async fn add_order_with_zero_target_work_rejected() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --hashprice 1000",
    );

    let response = router
        .add_order(&api::OrderRequest {
            target: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            target_work: HashDays(0.0),
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
#[timeout(120000)]
async fn add_order_response_amount_uses_price_and_dust_floor() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --hashprice 1000",
    );

    let (_, _, priced_amount) = add_order(
        &router,
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333",
        HashDays(2e15),
    )
    .await;
    assert_eq!(priced_amount, 2000);

    let (_, _, dust_amount) = add_order(
        &router,
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:4444",
        HashDays(1e12),
    )
    .await;
    assert_eq!(dust_amount, dust_limit(&descriptor));
    assert!(dust_amount > 1);
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
        "--start-diff 0.00001 --tick-interval 1 --hashprice 1000",
    );

    let status = router.get_status().await.unwrap();
    assert_eq!(status.order_count, 0);

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{pool_username}@{}", pool_a.stratum_endpoint()),
        HashDays(1e15),
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{pool_username}@{}", pool_b.stratum_endpoint()),
        HashDays(1e15),
    )
    .await;

    let status = router.get_status().await.unwrap();
    assert_eq!(status.order_count, 2);

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
                && status.order_count == 2
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
    assert_eq!(status.order_count, 2);
    assert_eq!(status.session_count, 3);

    drop(pool_a);

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status
                    .orders
                    .iter()
                    .filter(|o| o.status == OrderStatus::Active)
                    .count()
                    == 1
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
    assert_eq!(status.order_count, 2);
    assert_eq!(status.session_count, 3);

    drop(pool_b);

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status
                    .orders
                    .iter()
                    .filter(|o| o.status == OrderStatus::Active)
                    .count()
                    == 0
            {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Timeout waiting for all upstreams to disconnect");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.order_count, 2);

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
        "--start-diff 0.00001 --tick-interval 1 --hashprice 1000",
    );

    let status = router.get_status().await.unwrap();
    assert_eq!(status.order_count, 0);

    let response = router
        .add_order(&api::OrderRequest {
            target_work: HashDays(1e15),
            target: format!("{username}@127.0.0.1:1").parse().unwrap(),
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{username}@{}", pool_a.stratum_endpoint()),
        HashDays(1e15),
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{username}@{}", pool_b.stratum_endpoint()),
        HashDays(1e15),
    )
    .await;

    let status = router.get_status().await.unwrap();
    assert_eq!(status.order_count, 3);

    let response = router.cancel_order(9999).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let active_id = status
        .orders
        .iter()
        .find(|o| o.status == OrderStatus::Active)
        .unwrap()
        .id;
    let response = router.cancel_order(active_id).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let status = router.get_status().await.unwrap();
    assert_eq!(status.order_count, 3);
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
        "--tick-interval 1 --timeout 2 --hashprice 1000",
    );

    let (id, address, amount) = add_order(
        &router,
        &format!("{}@127.0.0.1:{stalled_port}", signet_username()),
        HashDays(1e15),
    )
    .await;

    pay_address(&wallet_bitcoind, &funding_descriptor, &address, amount).await;
    sleep(Duration::from_millis(1200)).await;

    let response = router.cancel_order(id).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    sleep(Duration::from_secs(3)).await;

    let status = router.get_status().await.unwrap();
    let order = status.orders.iter().find(|order| order.id == id).unwrap();

    assert_eq!(order.status, OrderStatus::Cancelled);
    assert_eq!(status.order_count, 1);

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
        "--start-diff 0.00001 --tick-interval 1 --hashprice 1000",
    );

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{username}@{}", pool_a.stratum_endpoint()),
        HashDays(1e15),
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{username}@{}", pool_b.stratum_endpoint()),
        HashDays(1e15),
    )
    .await;

    let status = router.get_status().await.unwrap();
    assert_eq!(status.order_count, 2);

    drop(pool_b);

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status
                    .orders
                    .iter()
                    .any(|o| o.status == OrderStatus::Disconnected)
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for order to be marked disconnected after upstream disconnect");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.order_count, 2);
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
        "--start-diff 0.00001 --tick-interval 1 --hashprice 1000",
    );

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{pool_username}@{}", pool.stratum_endpoint()),
        HashDays(1e-10),
    )
    .await;

    let status = router.get_status().await.unwrap();
    assert_eq!(status.order_count, 1);

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
    assert_eq!(status.order_count, 1);

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
    let miner_username = format!("{miner_address}.miner")
        .parse::<Username>()
        .unwrap();

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
        "--start-diff 0.00001 --tick-interval 1 --hashprice 1000",
    );

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{}@{}", upstream_username, pool_a.stratum_endpoint()),
        HashDays(1e15),
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{}@{}", upstream_username, pool_b.stratum_endpoint()),
        HashDays(1e15),
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
        "--start-diff 0.00001 --tick-interval 1 --hashprice 1000",
    );

    let username_a = signet_username();
    let username_b = "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.bar"
        .parse::<Username>()
        .unwrap();

    add_order(&router, &format!("{username_a}@foo:3333"), HashDays(1e15)).await;
    add_order(&router, &format!("{username_a}@foo:4444"), HashDays(1e15)).await;
    add_order(&router, &format!("{username_b}@foo:5555"), HashDays(1e15)).await;

    let all = router.list_orders(None).await.unwrap();
    assert_eq!(all.len(), 3);

    let filtered_a = router
        .list_orders(Some(
            &username_a.address().clone().assume_checked().to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(filtered_a.len(), 2);

    let filtered_b = router
        .list_orders(Some(
            &username_b.address().clone().assume_checked().to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(filtered_b.len(), 1);
}
