use crate::subcommand::server::database::Database;
use crate::subcommand::server::error::ServerResult;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use bip322::verify_simple;
use bitcoin::consensus::Decodable;
use bitcoin::{Address, Witness};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

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
    let account_data = match database.get_account(&address).await {
        Ok(account) => account,
        Err(_) => {
            return Ok((
                StatusCode::NOT_FOUND,
                Json(AccountResponse {
                    success: false,
                    remark: Some("Account not found".to_string()),
                }),
            )
                .into_response());
        }
    };

    let sparse_account = Account {
        btc_address: account_data.username,
        ln_address: account_data.lnurl.unwrap_or_default(),
        past_ln_addresses: vec![],
        total_diff: account_data.total_diff,
        payouts: vec![],
    };

    Ok(Json(sparse_account).into_response())
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
            verify_account_signature(&user_lookup.btc_address, &nonce, &signature)
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

    let signature_valid = verify_account_signature(
        &account_update.btc_address,
        &message,
        &account_update.signature,
    );

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

fn verify_account_signature(btc_address: &str, nonce: &str, signature_str: &str) -> bool {
    let message = nonce.to_string();
    verify_signature_for_message(btc_address, &message, signature_str)
}

fn verify_signature_for_message(btc_address: &str, message: &str, signature_str: &str) -> bool {
    let address = match Address::from_str(btc_address) {
        Ok(addr) => addr,
        Err(_) => return false,
    };

    let witness = match parse_witness(signature_str) {
        Ok(w) => w,
        Err(_) => return false,
    };

    verify_simple(address.assume_checked_ref(), message, witness).is_ok()
}

fn parse_witness(signature_str: &str) -> Result<Witness, String> {
    if let Ok(bytes) = hex::decode(signature_str)
        && let Ok(witness) = Witness::consensus_decode(&mut &bytes[..])
    {
        return Ok(witness);
    }

    // Disabled for now, but we could pass as base64
    /*if let Ok(bytes) = base64::decode(signature_str) {
        if let Ok(witness) = Witness::consensus_decode(&mut &bytes[..]) {
            return Ok(witness);
        }
    }*/

    if let Ok(witness) = Witness::consensus_decode(&mut signature_str.as_bytes()) {
        return Ok(witness);
    }

    Err("Failed to parse witness from signature string".to_string())
}
