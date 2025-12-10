use super::*;

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct Account {
    pub btc_address: String,
    pub ln_address: Option<String>,
    pub past_ln_addresses: Vec<String>,
    pub total_diff: i64,
    pub last_updated: Option<String>,
    /// Used as a general purpose sparse data storage for aspects of an account that are
    /// not critical to primary operations (mining pool).
    pub metadata: Option<serde_json::Value>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct AccountUpdate {
    pub btc_address: String,
    pub ln_address: String,
    pub signature: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct AccountMetadataUpdate {
    pub btc_address: String,
    pub metadata: serde_json::Value,
    pub signature: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct AccountResponse {
    pub success: bool,
    pub remark: Option<String>,
}

#[allow(deprecated)]
pub(crate) fn account_router(config: Arc<ServerConfig>, database: Database) -> Router {
    let mut router = Router::new()
        .route("/account/{address}", get(account_lookup))
        .route("/account/update", post(account_update))
        .route("/account/metadata", post(account_metadata_update));

    if let Some(token) = config.api_token() {
        router = router.layer(ValidateRequestHeaderLayer::bearer(token))
    };

    router.layer(Extension(database))
}

pub(crate) async fn account_lookup(
    Extension(database): Extension<Database>,
    Path(address): Path<String>,
) -> ServerResult<Response> {
    database
        .get_account(&address)
        .await?
        .ok_or_not_found(|| "Account")
        .map(|mut account| {
            account.past_ln_addresses.sort();
            Json(account)
        })
        .map(IntoResponse::into_response)
}

pub(crate) async fn account_update(
    Extension(database): Extension<Database>,
    Json(account_update): Json<AccountUpdate>,
) -> ServerResult<Response> {
    let signature_valid = verify_signature(
        &account_update.btc_address,
        &account_update.ln_address,
        &account_update.signature,
    );

    if !signature_valid {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    }

    database
        .update_account_lnurl(&account_update.btc_address, &account_update.ln_address)
        .await?
        .ok_or_not_found(|| "Account")
        .map(Json)
        .map(IntoResponse::into_response)
}

pub(crate) async fn account_metadata_update(
    Extension(database): Extension<Database>,
    Json(metadata_update): Json<AccountMetadataUpdate>,
) -> ServerResult<Response> {
    let message = serde_json::to_string(&metadata_update.metadata)
        .map_err(|e| anyhow!("Failed to serialize metadata: {}", e))?;

    let signature_valid = verify_signature(
        &metadata_update.btc_address,
        &message,
        &metadata_update.signature,
    );

    if !signature_valid {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    }

    database
        .update_account_metadata(&metadata_update.btc_address, &metadata_update.metadata)
        .await?
        .ok_or_not_found(|| "Account")
        .map(Json)
        .map(IntoResponse::into_response)
}

pub fn verify_signature(address: &str, message: &str, signature: &String) -> bool {
    match verify_simple_encoded(address, message, signature) {
        Ok(_) => true,
        Err(bip322::Error::WitnessMalformed { .. }) => {
            let secp = Secp256k1::verification_only();
            let address = Address::from_str(address)
                .expect("handled by default error")
                .assume_checked();

            let sig_bytes = match general_purpose::STANDARD.decode(signature) {
                Ok(bytes) => bytes,
                Err(_) => return false,
            };

            let msg_signature = MessageSignature::from_slice(&sig_bytes);

            if let Ok(sig_to_validate) = msg_signature {
                let msg_hash = bitcoin::sign_message::signed_msg_hash(message);
                sig_to_validate
                    .is_signed_by_address(&secp, &address, msg_hash)
                    .is_ok()
            } else {
                false
            }
        }
        Err(_) => false,
    }
}
