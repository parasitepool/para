use super::*;

pub struct TestAccount {
    pub legacy_address: String,
    pub wrapped_segwit_address: String,
    pub native_segwit_address: String,
    pub taproot_address: String,
    pub private_key: PrivateKey,
}

impl TestAccount {
    pub fn new() -> Self {
        let secp = Secp256k1::new();
        let (secret_key, _) = secp.generate_keypair(&mut bitcoin::secp256k1::rand::thread_rng());
        let private_key = PrivateKey::new(secret_key, Network::Testnet);

        let public_key = CompressedPublicKey::from_private_key(&secp, &private_key).unwrap();
        let legacy_address = Address::p2pkh(public_key, Network::Testnet).to_string();
        let wrapped_segwit_address = Address::p2shwpkh(&public_key, Network::Testnet).to_string();
        let native_segwit_address = Address::p2wpkh(&public_key, Network::Testnet).to_string();

        let untweaked_key = UntweakedPublicKey::from(secret_key.public_key(&secp));
        let taproot_address =
            Address::p2tr(&secp, untweaked_key, None, Network::Testnet).to_string();

        Self {
            legacy_address,
            wrapped_segwit_address,
            native_segwit_address,
            taproot_address,
            private_key,
        }
    }

    pub fn sign_update(&self, address: String, ln_address: &str) -> String {
        sign_simple_encoded(&address, ln_address, self.private_key.to_wif().as_str()).unwrap()
    }

    pub fn sign_update_legacy(&self, ln_address: &str) -> Result<String, Error> {
        let secp = Secp256k1::new();
        let message = ln_address;

        let msg_hash = bitcoin::sign_message::signed_msg_hash(message);
        let secp_message = bitcoin::secp256k1::Message::from_digest(msg_hash.to_byte_array());

        let signature = secp.sign_ecdsa_recoverable(&secp_message, &self.private_key.inner);
        let msg_signature = MessageSignature::new(signature, false);

        Ok(general_purpose::STANDARD.encode(msg_signature.serialize()))
    }
}

#[tokio::test]
async fn account_endpoints_behind_token() {
    let server = TestServer::spawn_with_db_args("--api-token verysecrettoken").await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();

    let response = server
        .get_json_async_raw(&format!("/account/{}", test_account.native_segwit_address))
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn account_not_found() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();

    let response = server
        .get_json_async_raw(&format!("/account/{}", test_account.native_segwit_address))
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn account_found_after_first_time_creation() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();
    let btc_address = test_account.native_segwit_address.clone();
    let ln_address = "lnurl0@test.gov";
    let signature = test_account.sign_update(btc_address.clone(), ln_address);

    let response: Account = server
        .post_json(
            "/account/update",
            &AccountUpdate {
                btc_address: btc_address.clone(),
                ln_address: ln_address.to_string(),
                signature,
            },
        )
        .await;

    assert_eq!(response.btc_address, btc_address);

    let account = server
        .get_json_async::<Account>(&format!("/account/{btc_address}"))
        .await;

    assert_eq!(account.btc_address, btc_address);
    assert_eq!(account.ln_address.unwrap_or_default(), "lnurl0@test.gov");
}

#[tokio::test]
async fn account_lnurl_new_account() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();
    let btc_address = test_account.native_segwit_address.clone();
    let ln_address = "lnurl1@test.com";
    let signature = test_account.sign_update(btc_address.clone(), ln_address);

    let response: Account = server
        .post_json(
            "/account/update",
            &AccountUpdate {
                btc_address: btc_address.clone(),
                ln_address: ln_address.to_string(),
                signature,
            },
        )
        .await;

    assert_eq!(response.btc_address, btc_address);

    let account = server
        .get_json_async::<Account>(&format!("/account/{btc_address}"))
        .await;

    assert_eq!(account.btc_address, btc_address);
    assert_eq!(account.ln_address, Some("lnurl1@test.com".to_string()));
    assert_eq!(account.past_ln_addresses.len(), 0);
    assert_eq!(account.total_diff, 0);
}

#[tokio::test]
async fn account_lnurl_existing_account_first_update() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();
    let btc_address = test_account.native_segwit_address.clone();

    let ln_address = "old_lnurl@test.com";
    let signature = test_account.sign_update(btc_address.clone(), ln_address);
    let _: Account = server
        .post_json(
            "/account/update",
            &AccountUpdate {
                btc_address: btc_address.clone(),
                ln_address: ln_address.to_string(),
                signature,
            },
        )
        .await;

    let ln_address = "new_lnurl@test.com";
    let signature = test_account.sign_update(btc_address.clone(), ln_address);
    let _: Account = server
        .post_json(
            "/account/update",
            &AccountUpdate {
                btc_address: btc_address.clone(),
                ln_address: ln_address.to_string(),
                signature,
            },
        )
        .await;

    let account = server
        .get_json_async::<Account>(&format!("/account/{btc_address}"))
        .await;

    assert_eq!(account.ln_address, Some("new_lnurl@test.com".to_string()));
    assert_eq!(account.past_ln_addresses.len(), 1);
    assert_eq!(account.past_ln_addresses[0], "old_lnurl@test.com");
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

    let account = database
        .get_account("user3@example.com")
        .await
        .unwrap()
        .unwrap();
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
    let signature =
        test_account.sign_update(test_account.native_segwit_address.clone(), ln_address);

    let update_request = AccountUpdate {
        btc_address: test_account.native_segwit_address.clone(),
        ln_address: ln_address.to_string(),
        signature,
    };

    let response: Account = server.post_json("/account/update", &update_request).await;
    assert_eq!(response.btc_address, test_account.native_segwit_address);

    let database = Database::new(db_url.clone()).await.unwrap();
    let account = database
        .get_account(&test_account.native_segwit_address)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.btc_address, test_account.native_segwit_address);
    assert_eq!(account.ln_address, Some(ln_address.to_string()));
    assert_eq!(account.past_ln_addresses.len(), 0);
    assert_eq!(account.total_diff, 0);
}

#[tokio::test]
async fn test_migrate_accounts_basic() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    insert_test_remote_shares(db_url.clone(), 10, 800000)
        .await
        .unwrap();

    let database = Database::new(db_url.clone()).await.unwrap();
    let rows_affected = database.migrate_accounts().await.unwrap();

    assert_eq!(rows_affected, 10, "spoofed user count wrong");

    for i in 0..10 {
        let username = format!("user_{}", i);
        let account = database.get_account(&username).await.unwrap().unwrap();
        assert_eq!(account.btc_address, username);
        assert_eq!(
            account.ln_address,
            Some(format!("lnurl{}@test.gov", i)),
            "migration failed to build accounts table"
        );
        assert_eq!(account.total_diff, 1000 + i);
    }

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
async fn account_address_type_taproot() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();
    let ln_address = "newuser@getalby.com";
    let signature = test_account.sign_update(test_account.taproot_address.clone(), ln_address);

    let update_request = AccountUpdate {
        btc_address: test_account.taproot_address.clone(),
        ln_address: ln_address.to_string(),
        signature,
    };

    let response: Account = server.post_json("/account/update", &update_request).await;
    assert_eq!(response.btc_address, test_account.taproot_address);

    let database = Database::new(db_url.clone()).await.unwrap();
    let account = database
        .get_account(&test_account.taproot_address)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.btc_address, test_account.taproot_address);
    assert_eq!(account.ln_address, Some(ln_address.to_string()));
    assert_eq!(account.past_ln_addresses.len(), 0);
    assert_eq!(account.total_diff, 0);
}

#[tokio::test]
async fn account_address_type_legacy() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();
    let ln_address = "newuser@getalby.com";
    let signature = test_account.sign_update_legacy(ln_address).unwrap();

    let update_request = AccountUpdate {
        btc_address: test_account.legacy_address.clone(),
        ln_address: ln_address.to_string(),
        signature,
    };

    let response: Account = server.post_json("/account/update", &update_request).await;
    assert_eq!(response.btc_address, test_account.legacy_address);

    let database = Database::new(db_url.clone()).await.unwrap();
    let account = database
        .get_account(&test_account.legacy_address)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.btc_address, test_account.legacy_address);
    assert_eq!(account.ln_address, Some(ln_address.to_string()));
    assert_eq!(account.past_ln_addresses.len(), 0);
    assert_eq!(account.total_diff, 0);
}

#[tokio::test]
async fn account_address_type_wrapped() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();
    let ln_address = "newuser@getalby.com";
    let signature =
        test_account.sign_update(test_account.wrapped_segwit_address.clone(), ln_address);

    let update_request = AccountUpdate {
        btc_address: test_account.wrapped_segwit_address.clone(),
        ln_address: ln_address.to_string(),
        signature,
    };

    let response: Account = server.post_json("/account/update", &update_request).await;
    assert_eq!(response.btc_address, test_account.wrapped_segwit_address);

    let database = Database::new(db_url.clone()).await.unwrap();
    let account = database
        .get_account(&test_account.wrapped_segwit_address)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.btc_address, test_account.wrapped_segwit_address);
    assert_eq!(account.ln_address, Some(ln_address.to_string()));
    assert_eq!(account.past_ln_addresses.len(), 0);
    assert_eq!(account.total_diff, 0);
}

#[tokio::test]
async fn account_address_type_native() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();
    let ln_address = "newuser@getalby.com";
    let signature =
        test_account.sign_update(test_account.native_segwit_address.clone(), ln_address);

    let update_request = AccountUpdate {
        btc_address: test_account.native_segwit_address.clone(),
        ln_address: ln_address.to_string(),
        signature,
    };

    let response: Account = server.post_json("/account/update", &update_request).await;
    assert_eq!(response.btc_address, test_account.native_segwit_address);

    let database = Database::new(db_url.clone()).await.unwrap();
    let account = database
        .get_account(&test_account.native_segwit_address)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.btc_address, test_account.native_segwit_address);
    assert_eq!(account.ln_address, Some(ln_address.to_string()));
    assert_eq!(account.past_ln_addresses.len(), 0);
    assert_eq!(account.total_diff, 0);
}

#[tokio::test]
async fn account_signature_invalid() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();
    let ln_address = "newuser@getalby.com";
    let signature = "invalid signature".to_string();

    let update_request = AccountUpdate {
        btc_address: test_account.native_segwit_address.clone(),
        ln_address: ln_address.to_string(),
        signature,
    };

    let response: Response = server
        .post_json_raw("/account/update", &update_request)
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
