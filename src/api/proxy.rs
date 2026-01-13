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

pub(crate) fn router(metrics: Arc<Metrics>) -> Router {
    Router::new()
        .route("/proxy/status", get(status))
        .with_state(metrics)
}

async fn status(State(metrics): State<Arc<Metrics>>) -> Json<Status> {
    Json(Status {
        upstream: metrics.upstream.upstream().to_string(),
        upstream_difficulty: metrics.upstream.upstream_difficulty().await.as_f64(),
        upstream_username: metrics.upstream.username().clone(),
        connected: metrics.upstream.is_connected(),
        enonce1: metrics.upstream.enonce1().clone(),
        enonce2_size: metrics.upstream.enonce2_size(),
    })
}
