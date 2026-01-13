use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub users: usize,
    pub workers: usize,
    pub connections: u64,
    pub hashrate_1m: HashRate,
    pub sps_1m: f64,
    pub accepted: u64,
    pub rejected: u64,
    pub blocks: u64,
    pub best_ever: Option<Difficulty>,
    pub last_share: Option<u64>,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDetail {
    pub address: String,
    pub hash_rate: HashRate,
    pub sps_1m: f64,
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: Option<Difficulty>,
    pub authorized: u64,
    pub workers: Vec<WorkerDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerDetail {
    pub name: String,
    pub hashrate_1m: HashRate,
    pub sps_1m: f64,
    pub accepted: u64,
    pub rejected: u64,
    pub best_ever: Option<Difficulty>,
}

pub(crate) fn router(metatron: Arc<Metatron>) -> Router {
    Router::new()
        .route("/pool/status", get(status))
        .route("/pool/users", get(users))
        .route("/pool/users/{address}", get(user))
        .with_state(metatron)
}

async fn status(State(metatron): State<Arc<Metatron>>) -> Json<Status> {
    Json(Status {
        hashrate_1m: metatron.hash_rate_1m(),
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

async fn users(State(metatron): State<Arc<Metatron>>) -> Json<Vec<String>> {
    Json(
        metatron
            .users()
            .iter()
            .map(|entry| entry.key().to_string())
            .collect(),
    )
}

async fn user(
    State(metatron): State<Arc<Metatron>>,
    Path(address): Path<Address<NetworkUnchecked>>,
) -> ServerResult<Response> {
    let address = address.assume_checked();

    let user = metatron
        .users()
        .get(&address)
        .ok_or_not_found(|| format!("User {address}"))?;

    Ok(Json(UserDetail {
        address: user.address.to_string(),
        hash_rate: user.hash_rate_1m(),
        sps_1m: user.sps_1m(),
        accepted: user.accepted(),
        rejected: user.rejected(),
        best_ever: user.best_ever(),
        authorized: user.authorized,
        workers: user
            .workers()
            .map(|worker| WorkerDetail {
                name: worker.workername().to_string(),
                hashrate_1m: worker.hash_rate_1m(),
                sps_1m: worker.sps_1m(),
                accepted: worker.accepted(),
                rejected: worker.rejected(),
                best_ever: worker.best_ever(),
            })
            .collect(),
    })
    .into_response())
}
