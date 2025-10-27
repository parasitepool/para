use crate::subcommand::server::database::Database;
use crate::subcommand::server::error::ServerResult;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct Account {
    pub(crate) btc_address: String,
    pub(crate) ln_address: String,
    pub(crate) past_ln_addresses: Vec<String>,
    pub(crate) total_diff: i64,
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
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct AccountUpdate {
    pub(crate) btc_address: String,
    pub(crate) ln_address: String,
    pub(crate) signature: String,
    pub(crate) nonce: i32,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub(crate) struct AccountResponse {
    pub(crate) success: bool,
    pub(crate) remark: Option<String>,
}

pub fn account_router() -> Router {
    Router::new()
        .route("/account/lookup", get(account_lookup))
        .route("/account/update", get(account_update))
}

// If accountlookup has an address not in our accounts table:
//     return an Account::Default() with btc_address set to the requested address
// If accountlookup is found, but has no signature:
//     return publicly consumable account details (dropping historical lnurls and payouts)
// If accountlookup is found, but signature is invalid:
//     return publicly consumable account details (dropping historical lnurls and payouts)
// If accountlookup is found and signature is valid (or is admin):
//     return everything
pub(crate) async fn account_lookup(
    Extension(database): Extension<Database>,
    Json(user_lookup): Json<AccountLookup>,
) -> ServerResult<Response> {
    todo!("Will return Account in response to AccountLookup");
    todo!("Response should be sparse (no past_ln, etc.) for AccountLookup without signature");
    Ok(Json(Account {
        btc_address: "bc1qplaceholder".to_string(),
        ln_address: "abcdefabc@parasite.wtf".to_string(),
        past_ln_addresses: vec!["cbafedcba@parasite.wtf".to_string()],
        total_diff: 0,
        payouts: vec![],
    })
    .into_response())
}

// Check if the signature provided is valid over "btc_address|ln_address|nonce"
// Update record if it is with the new ln_address
pub(crate) async fn account_update(
    Extension(database): Extension<Database>,
    Json(account_update): Json<AccountUpdate>,
) -> ServerResult<Response> {
    todo!("Will return AccountResponse in response to AccountUpdate");
    Ok(Json(AccountResponse {
        success: true,
        remark: None,
    })
    .into_response())
}
