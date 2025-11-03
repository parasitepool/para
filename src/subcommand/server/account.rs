use super::*;

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct Account {
    pub btc_address: String,
    pub ln_address: Option<String>,
    pub past_ln_addresses: Vec<String>,
    pub total_diff: i64,
    pub last_updated: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct AccountUpdate {
    pub btc_address: String,
    pub ln_address: String,
    pub signature: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct AccountResponse {
    pub success: bool,
    pub remark: Option<String>,
}

pub fn account_router() -> Router {
    Router::new()
        .route("/account/{address}", get(account_lookup))
        .route("/account/update", post(account_update))
}

pub(crate) async fn account_lookup(
    Extension(database): Extension<Database>,
    Path(address): Path<String>,
) -> ServerResult<Response> {
    database
        .get_account(&address)
        .await?
        .ok_or_not_found(|| "Account")
        .map(Json)
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
