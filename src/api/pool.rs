use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
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
    pub authorized: u64,
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

pub(crate) fn router(metatron: Arc<Metatron>) -> Router {
    Router::new()
        .route("/api/stats", get(stats))
        .route("/api/users", get(users))
        .route("/api/users/{address}", get(user))
        .with_state(metatron)
}

async fn stats(State(metatron): State<Arc<Metatron>>) -> Json<Stats> {
    Json(Stats {
        hash_rate_1m: metatron.hash_rate_1m(),
        sps_1m: metatron.sps_1m(),
        users: metatron.total_users(),
        workers: metatron.total_workers(),
        connections: metatron.total_connections(),
        accepted: metatron.accepted(),
        rejected: metatron.rejected(),
        blocks: metatron.total_blocks(),
        best_ever: metatron.best_ever(),
        last_share: metatron.last_share().map(|time| time.elapsed().as_secs()),
        uptime_secs: metatron.uptime().as_secs(),
    })
}

async fn users(State(metatron): State<Arc<Metatron>>) -> Json<Vec<UserSummary>> {
    Json(
        metatron
            .iter_users()
            .map(|(address, user)| UserSummary {
                address: address.to_string(),
                hash_rate: user.hash_rate_1m(),
                shares_per_second: user.sps_1m(),
                workers: user.worker_count(),
                accepted: user.accepted(),
                rejected: user.rejected(),
                best_ever: user.best_ever(),
            })
            .collect(),
    )
}

async fn user(
    State(metatron): State<Arc<Metatron>>,
    Path(address): Path<Address<NetworkUnchecked>>,
) -> ServerResult<Response> {
    let address = address.assume_checked();

    let user = metatron
        .get_user(&address)
        .ok_or_not_found(|| format!("User {address}"))?;

    Ok(Json(UserDetail {
        address: user.address.to_string(),
        hash_rate: user.hash_rate_1m(),
        shares_per_second: user.sps_1m(),
        accepted: user.accepted(),
        rejected: user.rejected(),
        best_ever: user.best_ever(),
        authorized: user.authorized,
        workers: user
            .workers()
            .map(|worker| WorkerSummary {
                name: worker.workername().to_string(),
                hash_rate: worker.hash_rate_1m(),
                shares_per_second: worker.sps_1m(),
                accepted: worker.accepted(),
                rejected: worker.rejected(),
                best_ever: worker.best_ever(),
            })
            .collect(),
    })
    .into_response())
}
