use {
    super::*,
    crate::subcommand::server::database::{HighestDiff, TeraShare},
};

pub(crate) fn share_difficulty_router(config: Arc<ServerConfig>, database: Database) -> Router {
    let mut router = Router::new()
        .route("/highestdiff/{blockheight}", get(highestdiff))
        .route(
            "/highestdiff/{blockheight}/user/{username}",
            get(highestdiff_by_user),
        )
        .route("/highestdiff/{blockheight}/all", get(highestdiff_all_users))
        .route("/terashares", get(get_tera_shares));

    if let Some(token) = config.api_token() {
        router = router.layer(bearer_auth(token))
    };

    router.layer(Extension(database))
}

/// Get highest difficulty share for a given blockheight
#[utoipa::path(
    get,
    path = "/highestdiff/{blockheight}",
    security(("api_token" = [])),
    params(
        ("blockheight" = i32, Path, description = "Block height")
    ),
    responses(
        (status = 200, description = "Highest difficulty share", body = HighestDiff),
        (status = 404, description = "No shares found"),
    ),
    tag = "sharediff"
)]
pub(crate) async fn highestdiff(
    Path(blockheight): Path<i32>,
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    database
        .get_highestdiff(blockheight)
        .await?
        .ok_or_not_found(|| "HighestDiff")
        .map(Json)
        .map(IntoResponse::into_response)
}

/// Get highest difficulty share for a user at a specific blockheight
#[utoipa::path(
    get,
    path = "/highestdiff/{blockheight}/user/{username}",
    security(("api_token" = [])),
    params(
        ("blockheight" = i32, Path, description = "Block height"),
        ("username" = String, Path, description = "Username")
    ),
    responses(
        (status = 200, description = "Highest difficulty share for user", body = HighestDiff),
        (status = 404, description = "No shares found"),
    ),
    tag = "sharediff"
)]
pub(crate) async fn highestdiff_by_user(
    Path((blockheight, username)): Path<(i32, String)>,
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    database
        .get_highestdiff_by_user(blockheight, &username)
        .await?
        .ok_or_not_found(|| "HighestDiff")
        .map(Json)
        .map(IntoResponse::into_response)
}

/// Get highest difficulty shares for all users at a blockheight
#[utoipa::path(
    get,
    path = "/highestdiff/{blockheight}/all",
    security(("api_token" = [])),
    params(
        ("blockheight" = i32, Path, description = "Block height")
    ),
    responses(
        (status = 200, description = "Highest difficulty shares for all users", body = Vec<HighestDiff>),
    ),
    tag = "sharediff"
)]
pub(crate) async fn highestdiff_all_users(
    Path(blockheight): Path<i32>,
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    Ok(Json(database.get_highestdiff_all_users(blockheight).await?).into_response())
}

/// Get tera shares with optional blockheight range and minimum difficulty
#[utoipa::path(
    get,
    path = "/terashares",
    security(("api_token" = [])),
    params(
        ("min_blockheight" = Option<i32>, Query, description = "Minimum block height (inclusive)"),
        ("max_blockheight" = Option<i32>, Query, description = "Maximum block height (exclusive)"),
        ("min_diff" = Option<i64>, Query, description = "Minimum difficulty threshold (default: 1000000000000)")
    ),
    responses(
        (status = 200, description = "List of tera shares by username", body = Vec<TeraShare>),
    ),
    tag = "sharediff"
)]
pub(crate) async fn get_tera_shares(
    Extension(database): Extension<Database>,
    Query(params): Query<HashMap<String, String>>,
) -> ServerResult<Response> {
    let min_blockheight = params
        .get("min_blockheight")
        .and_then(|v| v.parse::<i32>().ok());

    let max_blockheight = params
        .get("max_blockheight")
        .and_then(|v| v.parse::<i32>().ok());

    let min_diff = params
        .get("min_diff")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(1_000_000_000_000);

    Ok(Json(
        database
            .get_tera_shares(min_blockheight, max_blockheight, min_diff)
            .await?,
    )
    .into_response())
}
