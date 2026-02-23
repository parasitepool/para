use super::*;

pub(crate) struct Session {
    id: u64,
    enonce1: Extranonce,
    address: Address,
    workername: String,
    username: Username,
    version_mask: Option<Version>,
    stats: Mutex<Stats>,
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

    pub(crate) fn snapshot_stats(&self) -> Stats {
        self.stats.lock().clone()
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
