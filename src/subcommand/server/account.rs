use super::*;

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct Account {
    pub btc_address: String,
    pub ln_address: Option<String>,
    pub past_ln_addresses: Vec<String>,
    pub total_diff: i64,
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
        .await
        .ok_or_not_found()
        .map(Json)
        .map(IntoResponse::into_response)
}

// Check if the signature provided is valid over "btc_address|ln_address|nonce"
// Update record if it is with the new ln_address
pub(crate) async fn account_update(
    Extension(database): Extension<Database>,
    Json(account_update): Json<AccountUpdate>,
) -> ServerResult<Response> {
    let signature_valid = verify_simple_encoded(
        &account_update.btc_address,
        &account_update.ln_address,
        &account_update.signature,
    )
    .is_ok();

    if !signature_valid {
        return Ok(Json(AccountResponse {
            success: false,
            remark: Some("Invalid signature".to_string()),
        })
        .into_response());
    }

    database
        .update_account_lnurl(&account_update.btc_address, &account_update.ln_address)
        .await
        .ok_or_not_found()
        .map(Json)
        .map(IntoResponse::into_response)
}
