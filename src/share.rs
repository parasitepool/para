use super::*;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct Share {
    pub(crate) height: Option<u64>,
    pub(crate) job_id: JobId,
    pub(crate) workername: String,
    pub(crate) address: Address,
    pub(crate) client_addr: SocketAddr,
    pub(crate) user_agent: Option<String>,
    pub(crate) enonce1: Extranonce,
    pub(crate) enonce2: Extranonce,
    pub(crate) nonce: Nonce,
    pub(crate) ntime: Ntime,
    pub(crate) version_bits: Option<Version>,
    pub(crate) pool_diff: Difficulty,
    pub(crate) share_diff: Option<Difficulty>,
    pub(crate) blockhash: Option<BlockHash>,
    pub(crate) result: bool,
    pub(crate) reject_reason: Option<StratumError>,
    pub(crate) timestamp: Instant,
    pub(crate) created_at: u64,
}

impl Share {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        height: Option<u64>,
        job_id: JobId,
        workername: String,
        address: Address,
        client_addr: SocketAddr,
        user_agent: Option<String>,
        enonce1: Extranonce,
        enonce2: Extranonce,
        nonce: Nonce,
        ntime: Ntime,
        version_bits: Option<Version>,
        pool_diff: Difficulty,
        blockhash: Option<BlockHash>,
        reject_reason: Option<StratumError>,
    ) -> Self {
        Self {
            height,
            job_id,
            workername,
            address,
            client_addr,
            user_agent,
            enonce1,
            enonce2,
            nonce,
            ntime,
            version_bits,
            pool_diff,
            share_diff: blockhash.map(Difficulty::from),
            blockhash,
            result: reject_reason.is_none(),
            reject_reason,
            timestamp: Instant::now(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}
