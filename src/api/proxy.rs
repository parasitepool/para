use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub upstream_url: String,
    pub upstream_username: String,
    pub downstream_address: String,
    pub downstream_port: u16,
    pub connected: bool,
}

pub(crate) fn router(nexus: Arc<Nexus>) -> Router {
    Router::new()
        .route("/api/status", get(get_status))
        .with_state(nexus)
}

async fn get_status(State(nexus): State<Arc<Nexus>>) -> Json<Status> {
    Json(Status {
        upstream_url: nexus.upstream_url().to_string(),
        upstream_username: nexus.upstream_username().to_string(),
        downstream_address: nexus.downstream_address().to_string(),
        downstream_port: nexus.downstream_port(),
        connected: nexus.is_connected(),
    })
}
