use super::*;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct Share {
    pub height: u64,
    pub job_id: JobId,
    pub workername: String,
    pub address: Address,
    pub client_addr: SocketAddr,
    pub user_agent: Option<String>,
    pub enonce1: Extranonce,
    pub enonce2: String,
    pub nonce: Nonce,
    pub ntime: Ntime,
    pub version_bits: Option<Version>,
    pub diff: f64,
    pub sdiff: f64,
    pub hash: BlockHash,
    pub result: bool,
    pub reject_reason: Option<StratumError>,
    pub timestamp: Instant,
    pub created_at: u64,
}

impl Share {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accepted(
        height: u64,
        job_id: JobId,
        workername: String,
        address: Address,
        client_addr: SocketAddr,
        user_agent: Option<String>,
        enonce1: Extranonce,
        enonce2: String,
        nonce: Nonce,
        ntime: Ntime,
        version_bits: Option<Version>,
        diff: f64,
        sdiff: f64,
        hash: BlockHash,
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
            diff,
            sdiff,
            hash,
            result: true,
            reject_reason: None,
            timestamp: Instant::now(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rejected(
        height: u64,
        job_id: JobId,
        workername: String,
        address: Address,
        client_addr: SocketAddr,
        user_agent: Option<String>,
        enonce1: Extranonce,
        enonce2: String,
        nonce: Nonce,
        ntime: Ntime,
        version_bits: Option<Version>,
        diff: f64,
        sdiff: f64,
        hash: BlockHash,
        reason: StratumError,
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
            diff,
            sdiff,
            hash,
            result: false,
            reject_reason: Some(reason),
            timestamp: Instant::now(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}
