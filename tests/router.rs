use super::*;

fn generate_descriptor() -> String {
    CommandBuilder::new("wallet --chain regtest generate")
        .run_and_deserialize_output::<para::subcommand::wallet::generate::Output>()
        .descriptor
}

async fn fund_wallet(bitcoind: &Bitcoind, descriptor: &str) -> String {
    let directory = TempDir::new().unwrap();
    let data_dir = directory.path().to_str().unwrap();

    let address = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --data-dir {data_dir} \
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

async fn send_to_address_without_mining(
    bitcoind: &Bitcoind,
    funding_descriptor: &str,
    address: &str,
    amount: u64,
) -> bitcoin::Txid {
    let directory = TempDir::new().unwrap();
    let data_dir = directory.path().to_str().unwrap();

    CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --data-dir {data_dir} \
         --descriptor {funding_descriptor} \
         send --fee-rate 1 --address {address} --amount {amount}",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<para::subcommand::wallet::send::Output>()
    .txid
}

async fn pay_address(bitcoind: &Bitcoind, funding_descriptor: &str, address: &str, amount: u64) {
    send_to_address_without_mining(bitcoind, funding_descriptor, address, amount).await;

    generate_to_address(bitcoind, 1, address).await;
}

async fn assert_in_mempool(bitcoind: &Bitcoind, txid: bitcoin::Txid) {
    bitcoind
        .client()
        .unwrap()
        .call_raw::<serde_json::Value>("getmempoolentry", &[json!(txid.to_string())])
        .await
        .unwrap();
}

async fn mine_tx_to_address(bitcoind: &Bitcoind, txid: bitcoin::Txid, address: &str) {
    let blocks = bitcoind
        .client()
        .unwrap()
        .call_raw::<Vec<String>>("generatetoaddress", &[json!(1), json!(address)])
        .await
        .unwrap();

    let block = bitcoind
        .client()
        .unwrap()
        .call_raw::<serde_json::Value>("getblock", &[json!(blocks[0])])
        .await
        .unwrap();

    let txids = block
        .get("tx")
        .and_then(|txids| txids.as_array())
        .expect("mined block should include tx ids");

    assert!(
        txids
            .iter()
            .any(|mined| mined.as_str() == Some(&txid.to_string())),
        "expected transaction {txid} in mined block {}",
        blocks[0],
    );
}

async fn add_order(
    router: &TestRouter,
    target: &str,
    hash_days: HashDays,
    hash_price: HashPrice,
) -> (u32, String, u64) {
    let response = router
        .add_order(&api::OrderRequest {
            upstream_target: target.parse().unwrap(),
            hash_days,
            hash_price,
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

    let body: api::OrderResponse = response.json().await.unwrap();

    assert_eq!(location, format!("/api/router/order/{}", body.order_id));

    (
        body.order_id,
        body.payment_address.assume_checked().to_string(),
        body.payment_amount.to_sat(),
    )
}

async fn current_hash_price(router: &TestRouter) -> HashPrice {
    let hash_price = router.get_status().await.unwrap().hash_price;
    assert!(hash_price.to_sats() > 0);
    hash_price
}

#[tokio::test]
#[timeout(120000)]
async fn router_auth_tiers() {
    let bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn_with_probe_token(
        &descriptor,
        &bitcoind,
        "--http-api-token api --http-admin-token admin",
        Some("admin"),
    );

    let client = reqwest::Client::new();
    let status_url = format!("{}/api/router/status", router.api_endpoint());
    let users_url = format!("{}/api/router/users", router.api_endpoint());
    let system_url = format!("{}/api/system/status", router.api_endpoint());
    let cancel_url = format!("{}/api/router/order/999999/cancel", router.api_endpoint());
    let login_url = format!("{}/login", router.api_endpoint());
    let login_page = client.get(&login_url).send().await.unwrap();
    assert_eq!(login_page.status(), StatusCode::OK);
    assert!(login_page.text().await.unwrap().contains("login-form"));

    let home = client
        .get(router.api_endpoint())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(home.contains("navbar-login"));
    assert!(!home.contains("navbar-logout"));

    assert_eq!(
        client.get(&status_url).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
    );
    assert_eq!(
        client.get(&users_url).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED,
    );
    assert_eq!(
        client
            .get(&users_url)
            .bearer_auth("api")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
    );
    assert_eq!(
        client
            .get(&status_url)
            .bearer_auth("api")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
    );
    assert_eq!(
        client
            .get(&status_url)
            .bearer_auth("admin")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
    );
    assert_eq!(
        client
            .get(&system_url)
            .bearer_auth("api")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED,
    );
    assert_eq!(
        client
            .get(&system_url)
            .bearer_auth("admin")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
    );
    assert_eq!(
        client
            .post(&cancel_url)
            .bearer_auth("api")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED,
    );
    assert_eq!(
        client
            .post(cancel_url)
            .bearer_auth("admin")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND,
    );
    assert_eq!(
        client
            .post(&login_url)
            .json(&json!({ "token": "wrong" }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED,
    );

    let api_login = client
        .post(&login_url)
        .json(&json!({ "token": "api" }))
        .send()
        .await
        .unwrap();
    assert_eq!(api_login.status(), StatusCode::OK);
    let api_cookie = api_login
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    assert_eq!(
        client
            .get(&status_url)
            .header(reqwest::header::COOKIE, &api_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
    );
    let home = client
        .get(router.api_endpoint())
        .header(reqwest::header::COOKIE, &api_cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(home.contains("navbar-logout"));
    assert!(!home.contains("navbar-login"));

    let login_redirect = client
        .get(&login_url)
        .header(reqwest::header::COOKIE, &api_cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(login_redirect.contains("navbar-logout"));
    assert_eq!(
        client
            .get(&system_url)
            .header(reqwest::header::COOKIE, &api_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED,
    );

    let admin_login = client
        .post(login_url)
        .json(&json!({ "token": "admin" }))
        .send()
        .await
        .unwrap();
    assert_eq!(admin_login.status(), StatusCode::OK);
    let admin_cookie = admin_login
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    assert_eq!(
        client
            .get(system_url)
            .header(reqwest::header::COOKIE, admin_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
    );
}

async fn add_and_activate_order(
    router: &TestRouter,
    wallet_bitcoind: &Bitcoind,
    funding_descriptor: &str,
    target: &str,
    hashdays: HashDays,
    price: HashPrice,
) -> u32 {
    let (id, address, amount) = add_order(router, target, hashdays, price).await;

    pay_address(wallet_bitcoind, funding_descriptor, &address, amount).await;

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(orders) = router.list_orders(None).await
                && orders
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
async fn router() {
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
    assert_eq!(status.bucket_order_count, 0);
    let hash_price = status.hash_price;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{pool_username}@{}", pool_a.stratum_endpoint()),
        HashDays::new(1e5).unwrap(),
        hash_price,
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{pool_username}@{}", pool_b.stratum_endpoint()),
        HashDays::new(1e5).unwrap(),
        hash_price,
    )
    .await;

    let status = router.get_status().await.unwrap();
    assert_eq!(status.bucket_order_count, 2);

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
                && status.bucket_order_count == 2
                && status.downstream.session_count >= 3
            {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Timeout waiting for 2 slots and 3 sessions");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.bucket_order_count, 2);
    assert_eq!(status.downstream.session_count, 3);
    assert_eq!(status.downstream.user_count, 1);
    assert_eq!(status.downstream.worker_count, 1);

    let users = router.get_users().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(
        users[0].address.clone().assume_checked().to_string(),
        miner_address
    );
    assert_eq!(users[0].worker_count, 1);
    assert_eq!(users[0].session_count, 3);

    let matched = router
        .users_query("search=miner")
        .await
        .unwrap()
        .json::<Vec<api::UserSummary>>()
        .await
        .unwrap();

    assert_eq!(matched.len(), 1);

    let unmatched = router
        .users_query("search=zzzzzzzz")
        .await
        .unwrap()
        .json::<Vec<api::UserSummary>>()
        .await
        .unwrap();

    assert!(unmatched.is_empty());

    let invalid_limit = router.users_query("limit=foo").await.unwrap();
    assert_eq!(invalid_limit.status(), StatusCode::BAD_REQUEST);

    let user = router.get_user(&miner_address).await.unwrap();
    assert_eq!(
        user.address.clone().assume_checked().to_string(),
        miner_address
    );
    assert_eq!(user.session_count, 3);
    assert_eq!(user.workers.len(), 1);
    assert_eq!(user.workers[0].name, "miner");
    assert_eq!(user.sessions.len(), 3);

    drop(pool_a);

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.downstream.session_count >= 3
            {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Timeout waiting for miners to reconnect to remaining upstream");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.downstream.session_count, 3);

    drop(pool_b);

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.downstream.session_count == 0
            {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("Timeout waiting for all upstreams to disconnect");

    for mut miner in miners {
        miner.kill().unwrap();
        miner.wait().unwrap();
    }
}

#[tokio::test]
#[timeout(120000)]
async fn add_order_without_hashdays_rejected() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn(&descriptor, &wallet_bitcoind, "--start-diff 0.00001");

    let response = reqwest::Client::new()
        .post(format!("{}/api/router/order", router.api_endpoint()))
        .json(&json!({
            "upstream_target": {
                "endpoint": "bar:3333",
                "username": "foo"
            },
            "price": 1000,
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
#[timeout(120000)]
async fn add_order_with_zero_hashdays_rejected() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn(&descriptor, &wallet_bitcoind, "--start-diff 0.00001");

    let response = router
        .add_order(&api::OrderRequest {
            upstream_target: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            hash_days: HashDays::new(0.0).unwrap(),
            hash_price: current_hash_price(&router).await,
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.text().await.unwrap(), "hash days must be positive");
}

#[tokio::test]
#[timeout(120000)]
async fn add_order_price_overflow_rejected() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn(&descriptor, &wallet_bitcoind, "--start-diff 0.00001");

    let response = router
        .add_order(&api::OrderRequest {
            upstream_target: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            hash_days: HashDays::new(f64::MAX).unwrap(),
            hash_price: HashPrice::from_sats(u64::MAX),
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.text().await.unwrap(), "price calculation overflow");
}

#[tokio::test]
#[timeout(120000)]
async fn add_order_rejects_price_below_minimum() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn(&descriptor, &wallet_bitcoind, "--start-diff 0.00001");

    let response = router
        .add_order(&api::OrderRequest {
            upstream_target: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            hash_days: HashDays::new(2e5).unwrap(),
            hash_price: HashPrice::from_sats(0),
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(response.text().await.unwrap().contains("below minimum"));

    let hash_price = current_hash_price(&router).await;
    let hash_days = HashDays::new(2e5).unwrap();
    let (_, _, accepted_amount) = add_order(
        &router,
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333",
        hash_days,
        hash_price,
    )
    .await;
    assert_eq!(
        accepted_amount,
        hash_price.total(hash_days).unwrap().to_sat()
    );

    let response = router
        .add_order(&api::OrderRequest {
            upstream_target: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:4444"
                .parse()
                .unwrap(),
            hash_days: HashDays::new(1e-10).unwrap(),
            hash_price,
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.text().await.unwrap().contains("below dust limit"));
}

#[tokio::test]
#[timeout(120000)]
async fn order_detail() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn(&descriptor, &wallet_bitcoind, "--start-diff 0.00001");

    let hash_days = HashDays::new(1e5).unwrap();
    let (id, address, amount) = add_order(
        &router,
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333",
        hash_days,
        current_hash_price(&router).await,
    )
    .await;

    let detail = router.get_order(id).await.unwrap();

    assert_eq!(detail.id, id);
    assert_eq!(detail.status, OrderStatus::Pending);
    assert_eq!(detail.requested_hash_days, Some(hash_days));
    assert_eq!(
        detail
            .payment_address
            .expect("bucket has address")
            .assume_checked()
            .to_string(),
        address,
    );
    assert_eq!(
        detail.payment_amount.expect("bucket has amount").to_sat(),
        amount,
    );
    assert!(detail.created_at_height.is_some());
    assert_eq!(detail.upstream.accepted_shares, 0);
    assert!(detail.sessions.is_empty());
    assert_eq!(detail.downstream.accepted_shares, 0);
    assert_eq!(detail.downstream.rejected_shares, 0);
}

#[tokio::test]
#[timeout(120000)]
async fn order_activates_after_payment_output_is_spent_before_confirmation() {
    let pool_bitcoind = bitcoind();
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    let funding_address = fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let pool_username = signet_username();
    let miner_address = fund_wallet(&wallet_bitcoind, &generate_descriptor()).await;
    let miner_username = format!("{miner_address}.miner");

    let pool = TestPool::spawn_with_args(&pool_bitcoind, "--start-diff 0.00001");

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    let (id, address, amount) = add_order(
        &router,
        &format!("{pool_username}@{}", pool.stratum_endpoint()),
        HashDays::new(1e5).unwrap(),
        current_hash_price(&router).await,
    )
    .await;

    let parent_txid =
        send_to_address_without_mining(&wallet_bitcoind, &funding_descriptor, &address, amount)
            .await;
    assert_in_mempool(&wallet_bitcoind, parent_txid).await;

    timeout(Duration::from_secs(30), async {
        loop {
            let detail = router.get_order(id).await;
            let status = router.get_status().await;
            if let (Ok(detail), Ok(status)) = (detail, status)
                && detail.status == OrderStatus::InMempool
                && status.bucket_order_count == 0
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("unconfirmed full payment should mark the order in mempool without activating it");

    let child_amount = amount
        .checked_sub(10_000)
        .expect("test payment amount should leave room for fees");
    let directory = TempDir::new().unwrap();
    let data_dir = directory.path().to_str().unwrap();
    let child_txid = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --data-dir {data_dir} \
         --descriptor {descriptor} \
         send --fee-rate 1 --address {funding_address} --amount {child_amount}",
        wallet_bitcoind.rpc_port, wallet_bitcoind.rpc_user, wallet_bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<para::subcommand::wallet::send::Output>()
    .txid;
    assert_in_mempool(&wallet_bitcoind, child_txid).await;

    generate_to_address(&wallet_bitcoind, 1, &funding_address).await;

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(detail) = router.get_order(id).await
                && detail.status == OrderStatus::Active
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("order should activate after the spent payment output confirms");

    let mut miner = CommandBuilder::new(format!(
        "miner {} --mode continuous --username {} --cpu-cores 1",
        router.stratum_endpoint(),
        miner_username
    ))
    .spawn();

    timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(detail) = router.get_order(id).await
                && (detail.downstream.accepted_shares > 0
                    || detail.downstream.accepted_work.as_f64() > 0.0)
            {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("active order should receive miner work");

    miner.kill().unwrap();
    miner.wait().unwrap();
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
    assert_eq!(status.bucket_order_count, 0);
    let hash_price = status.hash_price;

    let response = router
        .add_order(&api::OrderRequest {
            hash_days: HashDays::new(1e5).unwrap(),
            upstream_target: format!("{username}@127.0.0.1:1").parse().unwrap(),
            hash_price,
        })
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{username}@{}", pool_a.stratum_endpoint()),
        HashDays::new(1e5).unwrap(),
        hash_price,
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{username}@{}", pool_b.stratum_endpoint()),
        HashDays::new(1e5).unwrap(),
        hash_price,
    )
    .await;

    let status = router.get_status().await.unwrap();
    assert_eq!(status.bucket_order_count, 2);

    let response = router.cancel_order(9999).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let active_id = router
        .list_orders(None)
        .await
        .unwrap()
        .iter()
        .find(|o| o.status == OrderStatus::Active)
        .unwrap()
        .id;
    let response = router.cancel_order(active_id).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let status = router.get_status().await.unwrap();
    assert_eq!(status.bucket_order_count, 1);
}

#[tokio::test]
#[timeout(120000)]
async fn cancelled_order_stays_cancelled_during_activation() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let stalled_port = allocate_port();
    let (accepted_tx, mut accepted_rx) = mpsc::channel(1);
    let stalled_server = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", stalled_port))
            .await
            .unwrap();
        let (_stream, _) = listener.accept().await.unwrap();
        accepted_tx.send(()).await.ok();
        sleep(Duration::from_secs(10)).await;
    });

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--tick-interval 1 --timeout 2",
    );

    let (id, address, amount) = add_order(
        &router,
        &format!("{}@127.0.0.1:{stalled_port}", signet_username()),
        HashDays::new(1e5).unwrap(),
        current_hash_price(&router).await,
    )
    .await;

    pay_address(&wallet_bitcoind, &funding_descriptor, &address, amount).await;

    timeout(Duration::from_secs(30), accepted_rx.recv())
        .await
        .expect("Timeout waiting for activation connection")
        .expect("stalled upstream should report activation connection");

    let response = router.cancel_order(id).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    sleep(Duration::from_secs(3)).await;

    let status = router.get_status().await.unwrap();
    let orders = router.list_orders(None).await.unwrap();
    let order = orders.iter().find(|order| order.id == id).unwrap();

    assert_eq!(order.status, OrderStatus::Cancelled);
    assert_eq!(order.review, Review::Flagged);
    assert_eq!(status.bucket_order_count, 0);

    stalled_server.abort();
}

async fn wait_for_status(router: &TestRouter, id: u32, expected: OrderStatus) {
    timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(order) = router.get_order(id).await
                && order.status == expected
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("order {id} did not reach status {expected:?}"));
}

async fn wait_for_review(router: &TestRouter, id: u32, expected: Review) {
    timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(order) = router.get_order(id).await
                && order.review == expected
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("order {id} did not reach review {expected:?}"));
}

#[tokio::test]
#[timeout(120000)]
async fn late_payment_flag_is_cleared_and_survives_restart() {
    let bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    let funding_address = fund_wallet(&bitcoind, &funding_descriptor).await;

    let router = TestRouter::spawn(&descriptor, &bitcoind, "--tick-interval 1");

    let (id, address, amount) = add_order(
        &router,
        &format!("{}@127.0.0.1:1", signet_username()),
        HashDays::new(1e5).unwrap(),
        current_hash_price(&router).await,
    )
    .await;

    generate_to_address(&bitcoind, 8, &funding_address).await;
    wait_for_status(&router, id, OrderStatus::Expired).await;

    pay_address(&bitcoind, &funding_descriptor, &address, amount).await;
    wait_for_review(&router, id, Review::Flagged).await;

    let refund_txid =
        send_to_address_without_mining(&bitcoind, &descriptor, &funding_address, amount / 2).await;
    mine_tx_to_address(&bitcoind, refund_txid, &funding_address).await;
    wait_for_review(&router, id, Review::Flagged).await;

    let response = router.clear_order(id).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    wait_for_review(&router, id, Review::Cleared).await;

    let router = router.restart(&descriptor, &bitcoind, "--tick-interval 1");

    let order = router.get_order(id).await.unwrap();
    assert_eq!(order.status, OrderStatus::Expired);
    assert_eq!(order.review, Review::Cleared);
}

#[tokio::test]
#[timeout(120000)]
async fn order_survives_upstream_bounce_and_drops_sessions() {
    let pool_bitcoind = bitcoind();
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let pool_username = signet_username();
    let miner_address = fund_wallet(&wallet_bitcoind, &generate_descriptor()).await;
    let miner_username = format!("{miner_address}.miner");

    let port = allocate_port();
    let pool = TestPool::spawn_on_port(&pool_bitcoind, port, "--start-diff 0.00001");

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    let id = add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{pool_username}@127.0.0.1:{port}"),
        HashDays::new(1e5).unwrap(),
        current_hash_price(&router).await,
    )
    .await;

    let mut miner = CommandBuilder::new(format!(
        "miner {} --mode continuous --username {} --cpu-cores 1",
        router.stratum_endpoint(),
        miner_username
    ))
    .spawn();

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.downstream.session_count >= 1
            {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("miner session should establish");

    drop(pool);

    timeout(Duration::from_secs(10), async {
        loop {
            let status = router.get_status().await;
            let detail = router.get_order(id).await;
            if let (Ok(status), Ok(detail)) = (status, detail)
                && detail.status == OrderStatus::Active
                && detail.sessions.is_empty()
                && status.downstream.session_count == 0
            {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("on upstream drop: order stays Active, sessions drop to zero");

    let _pool = TestPool::spawn_on_port(&pool_bitcoind, port, "--start-diff 0.00001");

    timeout(Duration::from_secs(30), async {
        loop {
            let detail = router.get_order(id).await.unwrap();
            if detail.status == OrderStatus::Active && !detail.sessions.is_empty() {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("order should reconnect once the upstream is back");

    miner.kill().unwrap();
    miner.wait().unwrap();
}

#[tokio::test]
#[timeout(120000)]
async fn order_fulfilled_on_hashdays_reached() {
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
        HashDays::new(1.0).unwrap(),
        current_hash_price(&router).await,
    )
    .await;

    let status = router.get_status().await.unwrap();
    assert_eq!(status.bucket_order_count, 1);

    let mut miner = CommandBuilder::new(format!(
        "miner {} --mode continuous --username {} --cpu-cores 1",
        router.stratum_endpoint(),
        miner_username
    ))
    .spawn();

    timeout(Duration::from_secs(60), async {
        loop {
            if let Ok(orders) = router.list_orders(None).await
                && orders.iter().any(|o| o.status == OrderStatus::Fulfilled)
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for order to be fulfilled");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.bucket_order_count, 0);

    let orders = router.list_orders(None).await.unwrap();
    let fulfilled = orders
        .iter()
        .find(|o| o.status == OrderStatus::Fulfilled)
        .unwrap();

    assert_eq!(
        fulfilled.requested_hash_days,
        Some(HashDays::new(1.0).unwrap())
    );
    assert!(fulfilled.delivered_hash_days >= HashDays::new(1.0).unwrap());

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
        "--start-diff 0.00001 --tick-interval 1",
    );
    let hash_price = current_hash_price(&router).await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{}@{}", upstream_username, pool_a.stratum_endpoint()),
        HashDays::new(1e5).unwrap(),
        hash_price,
    )
    .await;

    add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{}@{}", upstream_username, pool_b.stratum_endpoint()),
        HashDays::new(1e5).unwrap(),
        hash_price,
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

    client_a.disconnect().await;
    drop(events_a);
    client_b.disconnect().await;
    drop(events_b);
}

#[tokio::test]
#[timeout(120000)]
async fn filter_orders() {
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    let username_a = signet_username();
    let username_b = "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.bar"
        .parse::<Username>()
        .unwrap();
    let hash_price = current_hash_price(&router).await;

    let (id_a1, payment_a1, _) = add_order(
        &router,
        &format!("{username_a}@foo:3333"),
        HashDays::new(1e5).unwrap(),
        hash_price,
    )
    .await;

    let (id_a2, payment_a2, _) = add_order(
        &router,
        &format!("{username_a}@foo:4444"),
        HashDays::new(1e5).unwrap(),
        hash_price,
    )
    .await;

    let (id_b, payment_b, _) = add_order(
        &router,
        &format!("{username_b}@foo:5555"),
        HashDays::new(1e5).unwrap(),
        hash_price,
    )
    .await;

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

    let filtered_payment = router.list_orders(Some(&payment_a1)).await.unwrap();

    assert_eq!(filtered_payment.len(), 1);
    assert_eq!(filtered_payment[0].id, id_a1);

    let search_payment = router
        .list_orders_query(&format!("search={}", urlencoding::encode(&payment_a2)))
        .await
        .unwrap()
        .json::<Vec<api::OrderSummary>>()
        .await
        .unwrap();

    assert_eq!(search_payment.len(), 1);
    assert_eq!(search_payment[0].id, id_a2);

    let search_endpoint = router
        .list_orders_query(&format!("search={}", urlencoding::encode("foo:5555")))
        .await
        .unwrap()
        .json::<Vec<api::OrderSummary>>()
        .await
        .unwrap();

    assert_eq!(search_endpoint.len(), 1);
    assert_eq!(search_endpoint[0].id, id_b);

    let search_target = router
        .list_orders_query(&format!(
            "search={}",
            urlencoding::encode(&format!("{username_b}@foo:5555"))
        ))
        .await
        .unwrap()
        .json::<Vec<api::OrderSummary>>()
        .await
        .unwrap();

    assert_eq!(search_target.len(), 1);
    assert_eq!(search_target[0].id, id_b);

    let response = router.cancel_order(id_a1).await.unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let pending_or_cancelled = router
        .list_orders_query("status=pending&status=cancelled")
        .await
        .unwrap()
        .json::<Vec<api::OrderSummary>>()
        .await
        .unwrap();

    assert_eq!(pending_or_cancelled.len(), 3);

    let cancelled_payment_search = router
        .list_orders_query(&format!(
            "status=cancelled&search={}",
            urlencoding::encode(&payment_a1)
        ))
        .await
        .unwrap()
        .json::<Vec<api::OrderSummary>>()
        .await
        .unwrap();

    assert_eq!(cancelled_payment_search.len(), 1);
    assert_eq!(cancelled_payment_search[0].id, id_a1);

    let cancelled_other_payment_search = router
        .list_orders_query(&format!(
            "status=cancelled&search={}",
            urlencoding::encode(&payment_b)
        ))
        .await
        .unwrap()
        .json::<Vec<api::OrderSummary>>()
        .await
        .unwrap();

    assert!(cancelled_other_payment_search.is_empty());

    let review_clean = router
        .list_orders_query("review=clean")
        .await
        .unwrap()
        .json::<Vec<api::OrderSummary>>()
        .await
        .unwrap();

    assert_eq!(review_clean.len(), 3);

    let limited = router
        .list_orders_query("limit=1")
        .await
        .unwrap()
        .json::<Vec<api::OrderSummary>>()
        .await
        .unwrap();

    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].id, id_b);

    let invalid_status = router.list_orders_query("status=unknown").await.unwrap();

    assert_eq!(invalid_status.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[timeout(120000)]
async fn order_transitions_to_in_mempool_then_active() {
    let pool_bitcoind = bitcoind();
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    let funding_address = fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let pool = TestPool::spawn_with_args(&pool_bitcoind, "--start-diff 0.00001");
    let username = signet_username();

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    let (id, address, amount) = add_order(
        &router,
        &format!("{username}@{}", pool.stratum_endpoint()),
        HashDays::new(1e5).unwrap(),
        current_hash_price(&router).await,
    )
    .await;

    let detail = router.get_order(id).await.unwrap();
    assert_eq!(detail.status, OrderStatus::Pending);

    send_to_address_without_mining(&wallet_bitcoind, &funding_descriptor, &address, amount).await;

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(detail) = router.get_order(id).await
                && detail.status == OrderStatus::InMempool
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("unconfirmed payment should transition order to InMempool");

    generate_to_address(&wallet_bitcoind, 1, &funding_address).await;

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(detail) = router.get_order(id).await
                && detail.status == OrderStatus::Active
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("confirmed payment should transition order to Active");
}

#[tokio::test]
#[timeout(300000)]
async fn router_persists_order_stats_across_restart() {
    let pool_bitcoind = bitcoind();
    let wallet_bitcoind = spawn_regtest();
    let descriptor = generate_descriptor();
    let funding_descriptor = generate_descriptor();
    fund_wallet(&wallet_bitcoind, &funding_descriptor).await;

    let pool_username = signet_username();
    let miner_address = fund_wallet(&wallet_bitcoind, &generate_descriptor()).await;
    let miner_username = format!("{miner_address}.miner");

    let port = allocate_port();
    let _pool = TestPool::spawn_on_port(&pool_bitcoind, port, "--start-diff 0.00001");

    let router = TestRouter::spawn(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    let order_id = add_and_activate_order(
        &router,
        &wallet_bitcoind,
        &funding_descriptor,
        &format!("{pool_username}@127.0.0.1:{port}"),
        HashDays::new(1e5).unwrap(),
        current_hash_price(&router).await,
    )
    .await;

    let mut miner = CommandBuilder::new(format!(
        "miner --mode share-found --username {miner_username} {} --cpu-cores 1",
        router.stratum_endpoint()
    ))
    .spawn();

    miner.wait().unwrap();

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(detail) = router.get_order(order_id).await
                && detail.status == OrderStatus::Active
                && detail.upstream.accepted_shares >= 1
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("upstream should accept share before restart");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let router = router.restart(
        &descriptor,
        &wallet_bitcoind,
        "--start-diff 0.00001 --tick-interval 1",
    );

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(detail) = router.get_order(order_id).await
                && detail.status == OrderStatus::Active
                && detail.upstream.accepted_shares >= 1
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("upstream order stats should survive restart");

    let status = router.get_status().await.unwrap();
    assert_eq!(
        status.downstream.user_count, 1,
        "downstream user should survive restart"
    );
    assert_eq!(
        status.downstream.worker_count, 1,
        "downstream worker should survive restart"
    );
}
