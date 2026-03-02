use super::*;

const USER_METADATA_KEYS: &[&str] = &["is_private"];

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
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

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub struct AccountUpdate {
    pub btc_address: String,
    pub ln_address: String,
    pub signature: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub struct AccountMetadataUpdate {
    pub btc_address: String,
    pub metadata: serde_json::Value,
    pub signature: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub struct AccountResponse {
    pub success: bool,
    pub remark: Option<String>,
}

pub(crate) fn account_router(config: Arc<ServerConfig>, database: Database) -> axum::Router {
    let mut router = axum::Router::new()
        .route("/account/{address}", get(account_lookup))
        .route("/account/update", post(account_update))
        .route(
            "/account/metadata",
            post(account_metadata_update).layer(DefaultBodyLimit::max(1024)),
        );

    if let Some(token) = config.api_token() {
        router = router.layer(bearer_auth(token))
    };

    router.layer(Extension(database))
}

/// Look up account by BTC address
#[utoipa::path(
    get,
    path = "/account/{address}",
    security(("api_token" = [])),
    params(
        ("address" = String, Path, description = "BTC address")
    ),
    responses(
        (status = 200, description = "Account found", body = Account),
        (status = 404, description = "Account not found"),
    ),
    tag = "account"
)]
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

/// Update account lightning address
///
/// BIP322 is used for signing when supported by underlying address.
/// p2pkh falls back to ECDSA(secp256k1) signature over the message value
#[utoipa::path(
    post,
    path = "/account/update",
    security(("api_token" = [])),
    request_body = AccountUpdate,
    responses(
        (status = 200, description = "Account updated", body = Account),
        (status = 401, description = "Invalid signature"),
        (status = 404, description = "Account not found"),
    ),
    tag = "account"
)]
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

/// Update account metadata
#[utoipa::path(
    post,
    path = "/account/metadata",
    security(("api_token" = [])),
    request_body = AccountMetadataUpdate,
    responses(
        (status = 200, description = "Metadata updated", body = Account),
        (status = 401, description = "Invalid signature"),
        (status = 404, description = "Account not found"),
    ),
    tag = "account"
)]
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

    let Some(object) = metadata_update.metadata.as_object() else {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    };

    let filtered: serde_json::Map<String, serde_json::Value> = object
        .iter()
        .filter(|(key, _)| USER_METADATA_KEYS.contains(&key.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    if filtered.is_empty() {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    if !filtered.values().all(|v| v.is_boolean() || v.is_null()) {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    let filtered = serde_json::Value::Object(filtered);

    database
        .update_account_metadata(&metadata_update.btc_address, &filtered)
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
