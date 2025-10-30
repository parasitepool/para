use crate::test_psql::setup_test_schema;
use crate::test_server::TestServer;
use axum::http::StatusCode;
use bip322::sign_simple;
use bitcoin::consensus::Encodable;
use bitcoin::secp256k1::Secp256k1;
use bitcoin::{Address, CompressedPublicKey, Network, PrivateKey};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct TestAccount {
    pub btc_address: String,
    pub _private_key: PrivateKey,
    pub _address: Address,
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
            _private_key: private_key,
            _address: address,
        }
    }

    pub fn _generate_nonce(&self) -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let random = uuid::Uuid::new_v4().to_string();
        format!("{}-{}", timestamp, &random[0..8])
    }

    pub fn _sign_lookup(&self, nonce: &str) -> String {
        let witness =
            sign_simple(&self._address, nonce, self._private_key).expect("Failed to sign");

        let mut bytes = Vec::new();
        witness
            .consensus_encode(&mut bytes)
            .expect("Failed to encode witness");
        hex::encode(bytes)
    }

    pub fn _sign_update(&self, ln_address: &str, nonce: &str) -> String {
        let message = format!("{}|{}|{}", self.btc_address, ln_address, nonce);

        let witness =
            sign_simple(&self._address, &message, self._private_key).expect("Failed to sign");

        let mut bytes = Vec::new();
        witness
            .consensus_encode(&mut bytes)
            .expect("Failed to encode witness");
        hex::encode(bytes)
    }
}

#[tokio::test]
async fn test_account_lookup_not_found() {
    let server = TestServer::spawn_with_db().await;
    let db_url = server.database_url().unwrap();
    setup_test_schema(db_url.clone()).await.unwrap();

    let test_account = TestAccount::new();

    let response = server
        .get_json_async_raw(&format!("/account/{}", test_account.btc_address))
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
