use super::*;

pub struct TestAccount {
    pub btc_address: String,
    pub private_key: PrivateKey,
    pub address: Address,
}

impl TestAccount {
    pub fn new() -> Self {
        let secp = Secp256k1::new();
        let (secret_key, _) = secp.generate_keypair(&mut bitcoin::secp256k1::rand::thread_rng());
        let private_key = PrivateKey::new(secret_key, Network::Testnet);

        let _public_key = CompressedPublicKey::from_private_key(&secp, &private_key).unwrap();
        let address = Address::p2wpkh(&_public_key, Network::Testnet);

        Self {
            btc_address: address.to_string(),
            private_key,
            address,
        }
    }

    pub fn sign_update(&self, ln_address: &str) -> String {
        sign_simple_encoded(
            &self.address.to_string(),
            ln_address,
            self.private_key.to_wif().as_str(),
        )
        .unwrap()
    }
}

#[tokio::test]
async fn account_lookup_not_found() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();

    let response = server
        .get_json_async_raw(&format!("/account/{}", test_account.btc_address))
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn account_lookup_found() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_remote_shares(db_url.clone(), 1, 800000)
        .await
        .expect("Share to be inserted and user record updated");

    let response = server
        .get_json_async_raw(&format!("/account/{}", "user_0"))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let account = response.json::<Account>().await.unwrap();
    assert_eq!(account.btc_address, "user_0");
    assert_eq!(account.ln_address.unwrap_or_default(), "lnurl0@test.gov");
    assert_eq!(account.total_diff, 1000);
}

#[tokio::test]
async fn account_lnurl_new_account() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let database = Database::new(db_url.clone()).await.unwrap();

    let result = database
        .update_account_lnurl("user0", "lnurl1@test.com")
        .await;

    assert!(result.is_ok(), "Should create new account successfully");

    let account = database.get_account("user0").await.unwrap();
    assert_eq!(account.btc_address, "user0");
    assert_eq!(account.ln_address, Some("lnurl1@test.com".to_string()));
    assert_eq!(account.past_ln_addresses.len(), 0);
    assert_eq!(account.total_diff, 0);
}

#[tokio::test]
async fn account_lnurl_existing_account_first_update() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_account(
        db_url.clone(),
        "user1@example.com",
        Some("old_lnurl@test.com"),
        vec![],
        1000,
    )
    .await
    .unwrap();

    let database = Database::new(db_url.clone()).await.unwrap();

    let result = database
        .update_account_lnurl("user1@example.com", "new_lnurl@test.com")
        .await;

    assert!(result.is_ok(), "Should update account successfully");

    let account = database.get_account("user1@example.com").await.unwrap();
    assert_eq!(account.ln_address, Some("new_lnurl@test.com".to_string()));
    assert_eq!(account.past_ln_addresses.len(), 1);
    assert_eq!(account.past_ln_addresses[0], "old_lnurl@test.com");
    assert_eq!(account.total_diff, 1000);
}

#[tokio::test]
async fn account_lnurl_fifo_limit() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let past_addresses = vec![
        "past9@test.com".to_string(),
        "past8@test.com".to_string(),
        "past7@test.com".to_string(),
        "past6@test.com".to_string(),
        "past5@test.com".to_string(),
        "past4@test.com".to_string(),
        "past3@test.com".to_string(),
        "past2@test.com".to_string(),
        "past1@test.com".to_string(),
        "past0@test.com".to_string(),
    ];

    insert_test_account(
        db_url.clone(),
        "user3@example.com",
        Some("current@test.com"),
        past_addresses,
        750,
    )
    .await
    .unwrap();

    let database = Database::new(db_url.clone()).await.unwrap();

    database
        .update_account_lnurl("user3@example.com", "newest@test.com")
        .await
        .unwrap();

    let account = database.get_account("user3@example.com").await.unwrap();
    assert_eq!(account.ln_address, Some("newest@test.com".to_string()));
    assert_eq!(
        account.past_ln_addresses.len(),
        10,
        "Should limit to 10 entries"
    );
    assert_eq!(account.past_ln_addresses[0], "current@test.com");
    assert_eq!(account.past_ln_addresses[9], "past1@test.com");
    assert!(
        !account
            .past_ln_addresses
            .contains(&"past0@test.com".to_string()),
        "Oldest entry should be dropped"
    );
}

#[tokio::test]
async fn account_update_endpoint_new_account_with_signature() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();
    let ln_address = "newuser@getalby.com";
    let signature = test_account.sign_update(ln_address);

    let update_request = AccountUpdate {
        btc_address: test_account.btc_address.clone(),
        ln_address: ln_address.to_string(),
        signature,
    };

    let response: AccountResponse = server.post_json("/account/update", &update_request).await;

    assert!(response.success, "Account update should succeed");
    assert!(response.remark.is_none());

    let database = Database::new(db_url.clone()).await.unwrap();
    let account = database
        .get_account(&test_account.btc_address)
        .await
        .unwrap();
    assert_eq!(account.btc_address, test_account.btc_address);
    assert_eq!(account.ln_address, Some(ln_address.to_string()));
    assert_eq!(account.past_ln_addresses.len(), 0);
    assert_eq!(account.total_diff, 0);
}
