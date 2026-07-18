use super::*;

/// Metadata keys that hold a boolean (or null to clear).
const BOOL_METADATA_KEYS: &[&str] = &["is_private"];

/// Metadata keys that hold a free-form string (or null/empty to clear).
const STRING_METADATA_KEYS: &[&str] = &["display_name"];

/// Maximum length, in characters, of a user-chosen display name. Kept short so
/// names render cleanly in leaderboards alongside (never instead of) the address.
const DISPLAY_NAME_MAX_CHARS: usize = 24;

/// Normalize and validate a user-supplied display name.
///
/// Returns:
/// - `Ok(Some(name))` for an accepted, trimmed name,
/// - `Ok(None)` when the value is null or trims to empty (a request to clear it),
/// - `Err(())` when the value is present but invalid (wrong type, too long, or
///   contains control characters).
///
/// Names are intentionally *not* required to be unique: they are cosmetic
/// labels, and the account's BTC address remains its identity everywhere.
fn sanitize_display_name(value: &serde_json::Value) -> Result<Option<String>, ()> {
    if value.is_null() {
        return Ok(None);
    }

    let raw = value.as_str().ok_or(())?;
    let trimmed = raw.trim();

    if trimmed.is_empty() {
        return Ok(None);
    }

    if trimmed.chars().count() > DISPLAY_NAME_MAX_CHARS {
        return Err(());
    }

    // Reject control characters (newlines, tabs, etc.) to keep leaderboard
    // rendering and logs well-behaved. Display escaping is still the renderer's
    // job; this only blocks obviously abusive input.
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(());
    }

    Ok(Some(trimmed.to_string()))
}

/// Validate an incoming metadata patch against the allowlists, returning the
/// sanitized object to persist. Unknown keys are dropped. Returns `Err` (→ 400)
/// if a recognized key carries an invalid value or nothing valid remains.
fn validate_metadata(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, ()> {
    let mut out = serde_json::Map::new();

    for (key, value) in object {
        if BOOL_METADATA_KEYS.contains(&key.as_str()) {
            if value.is_boolean() || value.is_null() {
                out.insert(key.clone(), value.clone());
            } else {
                return Err(());
            }
        } else if STRING_METADATA_KEYS.contains(&key.as_str()) {
            match sanitize_display_name(value)? {
                Some(name) => {
                    out.insert(key.clone(), serde_json::Value::String(name));
                }
                // Explicit clear: store JSON null. Readers (get_account /
                // display_names) treat null-or-empty as "no name set".
                None => {
                    out.insert(key.clone(), serde_json::Value::Null);
                }
            }
        }
        // Unrecognized keys are silently ignored.
    }

    if out.is_empty() {
        return Err(());
    }

    Ok(serde_json::Value::Object(out))
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, ToSchema)]
pub struct Account {
    pub btc_address: String,
    /// Optional cosmetic display name, surfaced on leaderboards and account
    /// pages. Never unique and never a substitute for `btc_address`, which
    /// remains the account identity. Derived from `metadata.display_name`.
    pub display_name: Option<String>,
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

pub(crate) fn account_router(database: Database) -> axum::Router {
    axum::Router::new()
        .route("/account/{address}", get(account_lookup))
        .route("/account/update", post(account_update))
        .route(
            "/account/metadata",
            post(account_metadata_update).layer(DefaultBodyLimit::max(1024)),
        )
        .route("/names", get(names_lookup))
        .layer(from_extractor::<ApiAuth>())
        .layer(Extension(database))
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

    let Ok(filtered) = validate_metadata(object) else {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    };

    database
        .update_account_metadata(&metadata_update.btc_address, &filtered)
        .await?
        .ok_or_not_found(|| "Account")
        .map(Json)
        .map(IntoResponse::into_response)
}

/// Bulk lookup of display names.
///
/// Returns a JSON object mapping BTC address -> display name for every account
/// that has set one. Intended to be fetched once by front-ends (e.g. the
/// leaderboard) and merged client-side, so that a public dashboard can label
/// rows with names while still showing the underlying address.
#[utoipa::path(
    get,
    path = "/names",
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Map of address to display name"),
    ),
    tag = "account"
)]
pub(crate) async fn names_lookup(
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    let names = database.display_names().await?;
    Ok(Json(names).into_response())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_and_trims_a_normal_name() {
        assert_eq!(
            sanitize_display_name(&json!("  Satoshi  ")),
            Ok(Some("Satoshi".to_string()))
        );
    }

    #[test]
    fn null_or_empty_clears_the_name() {
        assert_eq!(sanitize_display_name(&json!(null)), Ok(None));
        assert_eq!(sanitize_display_name(&json!("   ")), Ok(None));
    }

    #[test]
    fn rejects_overlong_and_control_chars_and_non_strings() {
        let long: String = "a".repeat(DISPLAY_NAME_MAX_CHARS + 1);
        assert_eq!(sanitize_display_name(&json!(long)), Err(()));
        assert_eq!(sanitize_display_name(&json!("bad\nname")), Err(()));
        assert_eq!(sanitize_display_name(&json!(42)), Err(()));
    }

    #[test]
    fn counts_unicode_by_char_not_byte() {
        // 24 multibyte chars is fine even though it exceeds 24 bytes.
        let name = "é".repeat(DISPLAY_NAME_MAX_CHARS);
        assert_eq!(sanitize_display_name(&json!(name)), Ok(Some(name)));
    }

    #[test]
    fn validate_metadata_mixes_typed_keys_and_drops_unknowns() {
        let input = json!({
            "is_private": true,
            "display_name": " Alice ",
            "not_allowed": "ignored"
        });
        let out = validate_metadata(input.as_object().unwrap()).unwrap();
        assert_eq!(out["is_private"], json!(true));
        assert_eq!(out["display_name"], json!("Alice"));
        assert!(out.get("not_allowed").is_none());
    }

    #[test]
    fn validate_metadata_rejects_bad_types_and_empty_patches() {
        // is_private must be boolean/null.
        assert!(validate_metadata(json!({"is_private": "yes"}).as_object().unwrap()).is_err());
        // Nothing recognized -> 400.
        assert!(validate_metadata(json!({"unknown": 1}).as_object().unwrap()).is_err());
    }
}
