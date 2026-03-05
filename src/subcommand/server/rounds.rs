use super::*;

#[derive(sqlx::FromRow, Serialize, Deserialize, Debug, Clone, ToSchema)]
pub(crate) struct Round {
    pub(crate) blockheight: i32,
    pub(crate) blockhash: String,
    pub(crate) username: Option<String>,
    pub(crate) diff: Option<f64>,
    pub(crate) coinbasevalue: Option<i64>,
}

#[derive(sqlx::FromRow, Serialize, Deserialize, Debug, Clone, ToSchema)]
pub(crate) struct RoundParticipant {
    pub(crate) username: String,
    pub(crate) blocks_participated: i64,
    pub(crate) top_diff: f64,
}

pub(crate) fn rounds_router(config: Arc<ServerConfig>, database: Database) -> axum::Router {
    let mut router = axum::Router::new()
        .route("/rounds", get(rounds))
        .route("/rounds/current", get(round_current))
        .route("/rounds/{blockheight}", get(round))
        .route("/participants/{blockheight}", get(participants));

    if let Some(token) = config.api_token() {
        router = router.layer(bearer_auth(token))
    };

    router.layer(Extension(database))
}

#[utoipa::path(
    get,
    path = "/rounds",
    security(("api_token" = [])),
    responses(
        (status = 200, description = "All completed rounds", body = Vec<Round>),
    ),
    tag = "rounds"
)]
pub(crate) async fn rounds(Extension(database): Extension<Database>) -> ServerResult<Response> {
    Ok(Json(database.get_rounds().await?).into_response())
}

#[utoipa::path(
    get,
    path = "/rounds/current",
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Current in-progress round", body = Vec<RoundParticipant>),
    ),
    tag = "rounds"
)]
pub(crate) async fn round_current(
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    Ok(Json(database.get_round_participation(None).await?).into_response())
}

#[utoipa::path(
    get,
    path = "/rounds/{blockheight}",
    security(("api_token" = [])),
    params(
        ("blockheight" = i32, Path, description = "Block height of the found block ending this round")
    ),
    responses(
        (status = 200, description = "Round participants", body = Vec<RoundParticipant>),
    ),
    tag = "rounds"
)]
pub(crate) async fn round(
    Path(blockheight): Path<i32>,
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    Ok(Json(database.get_round_participation(Some(blockheight)).await?).into_response())
}

#[utoipa::path(
    get,
    path = "/participants/{blockheight}",
    security(("api_token" = [])),
    params(
        ("blockheight" = i32, Path, description = "Block height")
    ),
    responses(
        (status = 200, description = "Usernames who submitted shares at this blockheight", body = Vec<String>),
    ),
    tag = "rounds"
)]
pub(crate) async fn participants(
    Path(blockheight): Path<i32>,
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    Ok(Json(database.get_participants(blockheight).await?).into_response())
}
