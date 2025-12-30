use {
    super::*,
    axum::extract::{Json, Path, State},
    http_server::error::{OptionExt, ServerResult},
};

pub(crate) fn router(metatron: Arc<Metatron>) -> Router {
    Router::new()
        .route("/api/stats", get(stats))
        .route("/api/users", get(users))
        .route("/api/users/{address}", get(user))
        .with_state(metatron)
}

async fn stats(State(metatron): State<Arc<Metatron>>) -> ServerResult<Response> {
    Ok(Json(metatron.stats()).into_response())
}

async fn users(State(metatron): State<Arc<Metatron>>) -> ServerResult<Response> {
    Ok(Json(metatron.users()).into_response())
}

async fn user(
    State(metatron): State<Arc<Metatron>>,
    Path(address): Path<Address<NetworkUnchecked>>,
) -> ServerResult<Response> {
    let address = address.assume_checked();

    Ok(Json(
        metatron
            .user(&address)
            .ok_or_not_found(|| format!("User {address}"))?,
    )
    .into_response())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolStats {
    pub hash_rate_1m: HashRate,
    pub sps_1m: f64,
    pub users: usize,
    pub workers: usize,
    pub connections: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub blocks: u64,
    pub best_ever: f64,
    pub last_share: Option<u64>,
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
