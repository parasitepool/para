use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub upstream: String,
    pub upstream_difficulty: f64,
    pub upstream_username: Username,
    pub connected: bool,
    pub enonce1: Extranonce,
    pub enonce2_size: usize,
}

pub(crate) fn router(argus: Arc<Argus>) -> Router {
    Router::new()
        .route("/proxy/status", get(status))
        .with_state(argus)
}

async fn status(State(argus): State<Arc<Argus>>) -> Json<Status> {
    Json(Status {
        upstream: argus.nexus.upstream().to_string(),
        upstream_difficulty: argus.nexus.upstream_difficulty().await.as_f64(),
        upstream_username: argus.nexus.username().clone(),
        connected: argus.nexus.is_connected(),
        enonce1: argus.nexus.enonce1().clone(),
        enonce2_size: argus.nexus.enonce2_size(),
    })
}
