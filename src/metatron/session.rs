use super::*;

pub(crate) struct Session {
    pub(crate) id: u64,
    pub(crate) enonce1: Extranonce,
    pub(crate) address: Address,
    pub(crate) workername: String,
    pub(crate) username: Username,
    pub(crate) version_mask: Option<Version>,
    pub(crate) active: AtomicBool,
    pub(crate) deactivated_at: Mutex<Option<Instant>>,
    pub(crate) stats: Mutex<Stats>,
}

impl Session {
    pub(crate) fn new(
        id: u64,
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
            active: AtomicBool::new(true),
            deactivated_at: Mutex::new(None),
            stats: Mutex::new(Stats::new()),
        }
    }

    pub(crate) fn id(&self) -> u64 {
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

    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub(crate) fn deactivate(&self) {
        self.active.store(false, Ordering::Relaxed);
        *self.deactivated_at.lock() = Some(Instant::now());
    }

    pub(crate) fn deactivated_at(&self) -> Option<Instant> {
        *self.deactivated_at.lock()
    }

    pub(crate) fn record_accepted(&self, pool_diff: Difficulty, share_diff: Difficulty) {
        let now = Instant::now();
        let diff = pool_diff.as_f64();
        let mut stats = self.stats.lock();

        stats.accepted += 1;
        stats.dsps_1m.record(diff, now);
        stats.dsps_5m.record(diff, now);
        stats.dsps_15m.record(diff, now);
        stats.dsps_1hr.record(diff, now);
        stats.dsps_6hr.record(diff, now);
        stats.dsps_1d.record(diff, now);
        stats.dsps_7d.record(diff, now);
        stats.sps_1m.record(1.0, now);
        stats.sps_5m.record(1.0, now);
        stats.sps_15m.record(1.0, now);
        stats.sps_1hr.record(1.0, now);
        stats.total_work += diff;
        stats.last_share = Some(now);

        if stats.best_ever.is_none_or(|best| share_diff > best) {
            stats.best_ever = Some(share_diff);
        }
    }

    pub(crate) fn record_rejected(&self) {
        self.stats.lock().rejected += 1;
    }

    pub(crate) fn hashrate_1m(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_1m.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_5m(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_5m.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_15m(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_15m.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_1hr(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_1hr.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_6hr(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_6hr.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_1d(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_1d.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_7d(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps_7d.value_at(Instant::now()))
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.stats.lock().sps_1m.value_at(Instant::now())
    }

    pub(crate) fn sps_5m(&self) -> f64 {
        self.stats.lock().sps_5m.value_at(Instant::now())
    }

    pub(crate) fn sps_15m(&self) -> f64 {
        self.stats.lock().sps_15m.value_at(Instant::now())
    }

    pub(crate) fn sps_1hr(&self) -> f64 {
        self.stats.lock().sps_1hr.value_at(Instant::now())
    }

    pub(crate) fn accepted(&self) -> u64 {
        self.stats.lock().accepted
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.stats.lock().rejected
    }

    pub(crate) fn best_ever(&self) -> Option<Difficulty> {
        self.stats.lock().best_ever
    }

    pub(crate) fn last_share(&self) -> Option<Instant> {
        self.stats.lock().last_share
    }

    pub(crate) fn total_work(&self) -> f64 {
        self.stats.lock().total_work
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(id: u64, enonce1: &str) -> Session {
        Session::new(
            id,
            enonce1.parse().unwrap(),
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
                .parse::<Address<bitcoin::address::NetworkUnchecked>>()
                .unwrap()
                .assume_checked(),
            "foo".into(),
            Username::new("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.foo"),
            None,
        )
    }

    #[test]
    fn new_session_is_active() {
        let session = test_session(0, "deadbeef");

        assert!(session.is_active());
        assert!(session.deactivated_at().is_none());
    }

    #[test]
    fn deactivate_sets_deactivated_at() {
        let session = test_session(0, "deadbeef");

        session.deactivate();

        assert!(!session.is_active());
        assert!(session.deactivated_at().is_some());
    }

    #[test]
    fn deactivated_at_is_recent() {
        let before = Instant::now();
        let session = test_session(0, "deadbeef");
        session.deactivate();
        let after = Instant::now();

        let at = session.deactivated_at().unwrap();
        assert!(at >= before);
        assert!(at <= after);
    }
}
