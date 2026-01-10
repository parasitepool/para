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

pub(crate) fn router(nexus: Arc<Nexus>) -> Router {
    Router::new()
        .route("/api/status", get(get_status))
        .with_state(nexus)
}

async fn get_status(State(nexus): State<Arc<Nexus>>) -> Json<Status> {
    Json(Status {
        upstream: nexus.upstream().to_string(),
        upstream_difficulty: nexus.upstream_difficulty().await.as_f64(),
        upstream_username: nexus.username().clone(),
        connected: nexus.is_connected(),
        enonce1: nexus.enonce1().clone(),
        enonce2_size: nexus.enonce2_size(),
    })
}
