use super::*;

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct Account {
    pub btc_address: String,
    pub(crate) ln_address: String,
    pub(crate) past_ln_addresses: Vec<String>,
    pub total_diff: i64,
    pub(crate) payouts: Vec<HistoricalPayout>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct HistoricalPayout {
    pub(crate) amount: u32,
    pub(crate) allocated_diff: i64,
    pub(crate) block_start: u32,
    pub(crate) block_end: u32,
    pub(crate) status: String,
    pub(crate) failure_reason: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct AccountLookup {
    pub(crate) btc_address: String,
    pub(crate) signature: Option<String>,
    pub(crate) nonce: Option<String>,
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
        .route("/account/details", post(account_detail))
        .route("/account/update", post(account_update))
}

pub(crate) async fn account_lookup(
    Extension(database): Extension<Database>,
    Path(address): Path<String>,
) -> ServerResult<Response> {
    account_detail(
        Extension(database),
        Json(AccountLookup {
            btc_address: address,
            signature: None,
            nonce: None,
        }),
    )
    .await
}

// If accountlookup has an address not in our accounts table:
//     return an Account::Default() with btc_address set to the requested address
// If accountlookup is found, but has no signature:
//     return publicly consumable account details (dropping historical lnurls and payouts)
// If accountlookup is found, but signature is invalid:
//     return publicly consumable account details (dropping historical lnurls and payouts)
// If accountlookup is found and signature is valid (or is admin):
//     return everything
pub(crate) async fn account_detail(
    Extension(database): Extension<Database>,
    Json(user_lookup): Json<AccountLookup>,
) -> ServerResult<Response> {
    let account_data = match database.get_account(&user_lookup.btc_address).await {
        Ok(account) => account,
        Err(_) => {
            return Ok(Json(Account {
                btc_address: user_lookup.btc_address,
                ln_address: String::new(),
                past_ln_addresses: vec![],
                total_diff: 0,
                payouts: vec![],
            })
            .into_response());
        }
    };

    let signature_valid =
        if let (Some(signature), Some(nonce)) = (user_lookup.signature, user_lookup.nonce) {
            verify_simple_encoded(&user_lookup.btc_address, &nonce, &signature).is_ok()
        } else {
            false
        };

    if signature_valid {
        let payouts = database.get_account_payouts(account_data.id).await?;

        Ok(Json(Account {
            btc_address: account_data.username,
            ln_address: account_data.lnurl.unwrap_or_default(),
            past_ln_addresses: account_data.past_lnurls,
            total_diff: account_data.total_diff,
            payouts,
        })
        .into_response())
    } else {
        Ok(Json(Account {
            btc_address: account_data.username,
            ln_address: account_data.lnurl.unwrap_or_default(),
            past_ln_addresses: vec![],
            total_diff: account_data.total_diff,
            payouts: vec![],
        })
        .into_response())
    }
}

// Check if the signature provided is valid over "btc_address|ln_address|nonce"
// Update record if it is with the new ln_address
pub(crate) async fn account_update(
    Extension(database): Extension<Database>,
    Json(account_update): Json<AccountUpdate>,
) -> ServerResult<Response> {
    let message = format!(
        "{}|{}|{}",
        account_update.btc_address, account_update.ln_address, account_update.nonce
    );

    let signature_valid = verify_simple_encoded(
        &account_update.btc_address,
        &message,
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
