use {super::*, crate::subcommand::server::database::HighestDiff};

pub(crate) fn share_difficulty_router(config: Arc<ServerConfig>, database: Database) -> Router {
    let mut router = Router::new()
        .route("/highestdiff/{blockheight}", get(highestdiff))
        .route(
            "/highestdiff/{blockheight}/user/{username}",
            get(highestdiff_by_user),
        )
        .route("/highestdiff/{blockheight}/all", get(highestdiff_all_users));

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
