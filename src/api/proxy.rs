use {super::*, crate::sandalphon::Sandalphon, axum::extract::State};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub upstream_url: String,
    pub upstream_username: String,
    pub downstream_address: String,
    pub downstream_port: u16,
    pub connected: bool,
}

pub(crate) fn router(sandalphon: Arc<Sandalphon>) -> Router {
    Router::new()
        .route("/api/status", get(get_status))
        .with_state(sandalphon)
}

async fn get_status(State(sandalphon): State<Arc<Sandalphon>>) -> Json<Status> {
    Json(Status {
        upstream_url: sandalphon.upstream_url().to_string(),
        upstream_username: sandalphon.upstream_username().to_string(),
        downstream_address: sandalphon.downstream_address().to_string(),
        downstream_port: sandalphon.downstream_port(),
        connected: sandalphon.is_connected(),
    })
}
