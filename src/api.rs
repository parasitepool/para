use {
    super::*,
    axum::extract::{Json, Path, State},
};

pub(crate) fn router(metatron: Arc<Metatron>) -> Router {
    Router::new()
        .route("/api/stats", get(api_stats))
        .route("/api/users", get(api_users))
        .route("/api/users/{address}", get(api_user))
        .with_state(metatron)
}

async fn api_stats(State(metatron): State<Arc<Metatron>>) -> Json<PoolStats> {
    Json(metatron.stats())
}

async fn api_users(State(metatron): State<Arc<Metatron>>) -> Json<Vec<UserSummary>> {
    Json(metatron.users())
}

async fn api_user(
    State(metatron): State<Arc<Metatron>>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    match metatron.user(&address) {
        Some(user) => Ok(Json(user)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStats {
    pub hash_rate: HashRate,
    pub shares_per_second: f64,
    pub users: usize,
    pub workers: usize,
    pub connections: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub blocks: u64,
    pub best_ever: f64,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSummary {
    pub address: String,
    pub hash_rate: HashRate,
    pub shares_per_second: f64,
    pub workers: usize,
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDetail {
    pub address: String,
    pub hash_rate: HashRate,
    pub shares_per_second: f64,
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: f64,
    pub workers: Vec<WorkerSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSummary {
    pub name: String,
    pub hash_rate: HashRate,
    pub shares_per_second: f64,
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: f64,
}
