use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(u64);

impl SessionId {
    pub fn new(upstream_id: u32, counter: u32) -> Self {
        Self((upstream_id as u64) << 32 | counter as u64)
    }

    pub fn upstream_id(self) -> u32 {
        (self.0 >> 32) as u32
    }
}

impl Display for SessionId {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

pub(crate) struct Session {
    id: SessionId,
    enonce1: Extranonce,
    address: Address,
    workername: String,
    username: Username,
    version_mask: Option<Version>,
    stats: Mutex<Stats>,
}

impl Session {
    pub(crate) fn new(
        id: SessionId,
        enonce1: Extranonce,
        address: Address,
        workername: String,
        username: Username,
        version_mask: Option<Version>,
    ) -> Self {
        Self {
            id,
            enonce1,
            address,
            workername,
            username,
            version_mask,
            stats: Mutex::new(Stats::new()),
        }
    }

    pub(crate) fn id(&self) -> SessionId {
        self.id
    }

    pub(crate) fn enonce1(&self) -> &Extranonce {
        &self.enonce1
    }

    pub(crate) fn address(&self) -> &Address {
        &self.address
    }

    pub(crate) fn workername(&self) -> &str {
        &self.workername
    }

    pub(crate) fn username(&self) -> &Username {
        &self.username
    }

    pub(crate) fn version_mask(&self) -> Option<Version> {
        self.version_mask
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        self.stats.lock().last_share
    }

    pub(crate) fn is_idle(&self, now: Instant) -> bool {
        self.last_share()
            .is_none_or(|last| now.duration_since(last).as_secs() > 60)
    }

    pub(crate) fn snapshot(&self) -> Stats {
        self.stats.lock().clone()
    }

    pub(crate) fn record_accepted(&self, pool_diff: Difficulty, share_diff: Difficulty) {
        let now = Instant::now();
        self.stats
            .lock()
            .record_accepted(pool_diff, share_diff, now);
    }

    pub(crate) fn record_rejected(&self, pool_diff: Difficulty) {
        self.stats.lock().record_rejected(pool_diff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_round_trip() {
        #[track_caller]
        fn case(upstream_id: u32, counter: u32) {
            let id = SessionId::new(upstream_id, counter);
            assert_eq!(id.upstream_id(), upstream_id);
        }

        case(0, 0);
        case(0, 1);
        case(1, 0);
        case(7, 42);
        case(u32::MAX, u32::MAX);
    }

    #[test]
    fn session_id_uniqueness() {
        let a = SessionId::new(0, 1);
        let b = SessionId::new(1, 0);
        assert_ne!(a, b);
    }
}
