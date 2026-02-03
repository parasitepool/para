use {
    super::*,
    crate::subcommand::server::{
        database::{FailedPayout, Payout, PendingPayout, Split, UpdatePayoutStatusRequest},
        templates::simulate_payouts::SimulatePayoutsHtml,
    },
};

pub(crate) fn payouts_router(config: Arc<ServerConfig>, database: Database) -> Router {
    let mut router = Router::new()
        .route("/payouts", get(payouts_all))
        .route("/payouts/failed", get(payouts_failed))
        .route("/payouts/simulate", get(payouts_simulate))
        .route("/payouts/{blockheight}", get(payouts))
        .route("/payouts/update", post(update_payout_status))
        .route(
            "/payouts/range/{start_height}/{end_height}",
            get(payouts_range),
        )
        .route(
            "/payouts/range/{start_height}/{end_height}/user/{username}",
            get(user_payout_range),
        )
        .route("/split", get(open_split))
        .route("/split/{blockheight}", get(sat_split))
        .layer(Extension(database));

    if let Some(token) = config.admin_token() {
        router = router.layer(bearer_auth(token))
    };

    router.layer(Extension(config))
}

/// Get all pending and failed payouts
#[utoipa::path(
    get,
    path = "/payouts",
    security(("admin_token" = [])),
    params(
        ("format" = Option<String>, Query, description = "Set to 'json' for JSON response")
    ),
    responses(
        (status = 200, description = "List of pending payouts", body = Vec<PendingPayout>),
    ),
    tag = "payouts"
)]
pub(crate) async fn payouts_all(
    Extension(config): Extension<Arc<ServerConfig>>,
    Extension(database): Extension<Database>,
    Query(params): Query<HashMap<String, String>>,
) -> ServerResult<Response> {
    let pending = database.get_pending_payouts().await?;

    let format_json = params.get("format").map(|f| f == "json").unwrap_or(false);
    if format_json {
        Ok(Json(&pending).into_response())
    } else {
        let failed = database.get_failed_payouts().await?;

        Ok(PayoutsHtml { pending, failed }
            .page(config.domain())
            .into_response())
    }
}

/// Get all failed payouts
#[utoipa::path(
    get,
    path = "/payouts/failed",
    security(("admin_token" = [])),
    responses(
        (status = 200, description = "List of failed payouts", body = Vec<FailedPayout>),
    ),
    tag = "payouts"
)]
pub(crate) async fn payouts_failed(
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    Ok(Json(database.get_failed_payouts().await?).into_response())
}

/// Simulate payouts as if a block was found now
#[utoipa::path(
    get,
    path = "/payouts/simulate",
    security(("admin_token" = [])),
    params(
        ("coinbase_value" = Option<i64>, Query, description = "Coinbase value in sats (default: 312500000 for 3.125 BTC)"),
        ("winner_address" = Option<String>, Query, description = "Username of the block winner to exclude from payouts"),
        ("format" = Option<String>, Query, description = "Response format: 'json', 'csv', or HTML (default)")
    ),
    responses(
        (status = 200, description = "Simulated payouts", body = Vec<PendingPayout>),
    ),
    tag = "payouts"
)]
pub(crate) async fn payouts_simulate(
    Extension(config): Extension<Arc<ServerConfig>>,
    Extension(database): Extension<Database>,
    Query(params): Query<HashMap<String, String>>,
) -> ServerResult<Response> {
    let coinbase_value: i64 = params
        .get("coinbase_value")
        .and_then(|v| v.parse().ok())
        .unwrap_or(312_500_000); // 3.125 BTC, current coinbase value
    let winner_address = params
        .get("winner_address")
        .map_or("", |user| user.as_str());

    let payouts = database
        .get_simulated_payouts(coinbase_value, winner_address)
        .await?;

    let format = params.get("format").map(|f| f.as_str()).unwrap_or("html");

    match format {
        "json" => Ok(Json(&payouts).into_response()),
        "csv" => {
            let mut csv = String::from("lightning_address,bitcoin_address,amount_sats\n");
            for payout in &payouts {
                csv.push_str(&format!(
                    "{},{},{}\n",
                    payout.ln_address, payout.btc_address, payout.amount_sats
                ));
            }
            Ok(([(CONTENT_TYPE, "text/csv; charset=utf-8")], csv).into_response())
        }
        _ => Ok(SimulatePayoutsHtml {
            payouts,
            coinbase_value,
            winner_address: winner_address.to_string(),
        }
        .page(config.domain())
        .into_response()),
    }
}

/// Get payouts for a specific block height
#[utoipa::path(
    get,
    path = "/payouts/{blockheight}",
    security(("admin_token" = [])),
    params(
        ("blockheight" = u32, Path, description = "Block height")
    ),
    responses(
        (status = 200, description = "Payouts for block", body = Vec<Payout>),
    ),
    tag = "payouts"
)]
pub(crate) async fn payouts(
    Path(blockheight): Path<u32>,
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    Ok(Json(
        database
            .get_payouts(blockheight.try_into().unwrap(), "no filter address".into())
            .await?,
    )
    .into_response())
}

/// Get current open split
#[utoipa::path(
    get,
    path = "/split",
    security(("admin_token" = [])),
    responses(
        (status = 200, description = "Current split data", body = Vec<Split>),
    ),
    tag = "payouts"
)]
pub(crate) async fn open_split(Extension(database): Extension<Database>) -> ServerResult<Response> {
    Ok(Json(database.get_split().await?).into_response())
}

/// Get sat split for a specific block
#[utoipa::path(
    get,
    path = "/split/{blockheight}",
    security(("admin_token" = [])),
    params(
        ("blockheight" = u32, Path, description = "Block height")
    ),
    responses(
        (status = 200, description = "Sat split for block", body = SatSplit),
        (status = 404, description = "Block not found"),
    ),
    tag = "payouts"
)]
pub(crate) async fn sat_split(
    Path(blockheight): Path<u32>,
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    if blockheight == 0 {
        return Err(ServerError::NotFound("block not mined by parasite".into()));
    }

    let Some((blockheight, blockhash, coinbasevalue, _, username)) = database
        .get_total_coinbase(blockheight.try_into().unwrap())
        .await?
    else {
        return Err(ServerError::NotFound("block not mined by parasite".into()));
    };

    let total_payment_amount = coinbasevalue.saturating_sub(COIN_VALUE.try_into().unwrap());

    let payouts = database.get_payouts(blockheight, username).await?;

    let mut payments = Vec::new();
    for payout in payouts {
        if let Some(lnurl) = payout.lnurl {
            payments.push(Payment {
                lightning_address: lnurl,
                amount: (total_payment_amount / payout.total_shares) * payout.payable_shares,
            });
        }
    }

    Ok(Json(SatSplit {
        block_height: blockheight,
        block_hash: blockhash,
        total_payment_amount,
        payments,
    })
    .into_response())
}

/// Get payouts for a range of blocks
#[utoipa::path(
    get,
    path = "/payouts/range/{start_height}/{end_height}",
    security(("admin_token" = [])),
    params(
        ("start_height" = u32, Path, description = "Start block height"),
        ("end_height" = u32, Path, description = "End block height"),
        ("excluded" = Option<String>, Query, description = "Comma-separated list of usernames to exclude")
    ),
    responses(
        (status = 200, description = "Payouts for block range", body = Vec<Payout>),
    ),
    tag = "payouts"
)]
pub(crate) async fn payouts_range(
    Path((start_height, end_height)): Path<(u32, u32)>,
    Query(params): Query<HashMap<String, String>>,
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    let excluded_usernames = exclusion_list_from_params(params);

    Ok(Json(
        database
            .get_payouts_range(
                start_height.try_into().unwrap(),
                end_height.try_into().unwrap(),
                excluded_usernames,
            )
            .await?,
    )
    .into_response())
}

/// Get payouts for a specific user in a block range
#[utoipa::path(
    get,
    path = "/payouts/range/{start_height}/{end_height}/user/{username}",
    security(("admin_token" = [])),
    params(
        ("start_height" = u32, Path, description = "Start block height"),
        ("end_height" = u32, Path, description = "End block height"),
        ("username" = String, Path, description = "Username to filter"),
        ("excluded" = Option<String>, Query, description = "Comma-separated list of usernames to exclude")
    ),
    responses(
        (status = 200, description = "Payouts for user in block range", body = Vec<Payout>),
    ),
    tag = "payouts"
)]
pub(crate) async fn user_payout_range(
    Path((start_height, end_height, username)): Path<(u32, u32, String)>,
    Query(params): Query<HashMap<String, String>>,
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    let excluded_usernames = exclusion_list_from_params(params);

    Ok(Json(
        database
            .get_user_payout_range(
                start_height.try_into().unwrap(),
                end_height.try_into().unwrap(),
                username,
                excluded_usernames,
            )
            .await?,
    )
    .into_response())
}

/// Update payout status
#[utoipa::path(
    post,
    path = "/payouts/update",
    security(("admin_token" = [])),
    request_body = UpdatePayoutStatusRequest,
    responses(
        (status = 200, description = "Status updated successfully"),
    ),
    tag = "payouts"
)]
pub(crate) async fn update_payout_status(
    Extension(database): Extension<Database>,
    Json(request): Json<UpdatePayoutStatusRequest>,
) -> ServerResult<Response> {
    let rows_affected = database
        .update_payout_status(
            &request.payout_ids,
            &request.status,
            request.failure_reason.as_deref(),
        )
        .await?;

    Ok(Json(json!({
        "status": "OK",
        "rows_affected": rows_affected,
    }))
    .into_response())
}
