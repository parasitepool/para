use {super::*, crate::subcommand::server::database::BlockContributors};

pub(crate) fn blocks_router(config: Arc<ServerConfig>, database: Database) -> axum::Router {
    let mut router = axum::Router::new().route(
        "/blocks/latest/contributors",
        get(latest_block_contributors),
    );

    if let Some(token) = config.api_token() {
        router = router.layer(bearer_auth(token))
    };

    router.layer(Extension(database))
}

#[utoipa::path(
    get,
    path = "/blocks/latest/contributors",
    security(("api_token" = [])),
    responses(
        (status = 200, description = "Contributors to the latest found block", body = BlockContributors),
        (status = 404, description = "No blocks found"),
    ),
    tag = "blocks"
)]
pub(crate) async fn latest_block_contributors(
    Extension(database): Extension<Database>,
) -> ServerResult<Response> {
    database
        .get_latest_block_contributors()
        .await?
        .ok_or_not_found(|| "BlockContributors")
        .map(Json)
        .map(IntoResponse::into_response)
}
