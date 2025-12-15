use super::*;

pub(crate) fn share_difficulty_router(state: ServerState, database: Database) -> Router {
    let mut router = Router::new()
        .route("/highestdiff/{blockheight}", get(highestdiff))
        .route(
            "/highestdiff/{blockheight}/user/{username}",
            get(highestdiff_by_user),
        )
        .route("/highestdiff/{blockheight}/all", get(highestdiff_all_users));

    if let Some(token) = state.config.api_token(&state.settings) {
        router = router.layer(bearer_auth(&token))
    };

    router.layer(Extension(database))
}

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

pub(crate) async fn highestdiff_all_users(
    Path(blockheight): Path<i32>,
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    Ok(Json(database.get_highestdiff_all_users(blockheight).await?).into_response())
}
