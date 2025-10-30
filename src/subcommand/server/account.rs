use super::*;

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct Account {
    pub btc_address: String,
    pub(crate) ln_address: Option<String>,
    pub(crate) past_ln_addresses: Vec<String>,
    pub total_diff: i64,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct AccountUpdate {
    pub(crate) btc_address: String,
    pub(crate) ln_address: String,
    pub(crate) signature: String,
    pub(crate) nonce: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct AccountResponse {
    pub(crate) success: bool,
    pub(crate) remark: Option<String>,
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
    match database.get_account(&address).await {
        Ok(account) => Ok(Json(account).into_response()),
        Err(_) => Ok((
            StatusCode::NOT_FOUND,
            Json(AccountResponse {
                success: false,
                remark: Some("Account not found".to_string()),
            }),
        )
            .into_response()),
    }
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

    match database
        .update_account_lnurl(&account_update.btc_address, &account_update.ln_address)
        .await
    {
        Ok(_) => Ok(Json(AccountResponse {
            success: true,
            remark: None,
        })
        .into_response()),
        Err(err) => Ok(Json(AccountResponse {
            success: false,
            remark: Some(format!("Failed to update account: {}", err)),
        })
        .into_response()),
    }
}
