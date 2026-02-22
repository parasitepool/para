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
    pub(crate) accepted: AtomicU64,
    pub(crate) rejected: AtomicU64,
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
            stats: Mutex::new(Stats {
                dsps_1m: DecayingAverage::new(Duration::from_mins(1)),
                dsps_5m: DecayingAverage::new(Duration::from_mins(5)),
                dsps_15m: DecayingAverage::new(Duration::from_mins(15)),
                dsps_1hr: DecayingAverage::new(Duration::from_hours(1)),
                dsps_6hr: DecayingAverage::new(Duration::from_hours(6)),
                dsps_1d: DecayingAverage::new(Duration::from_hours(24)),
                dsps_7d: DecayingAverage::new(Duration::from_hours(24 * 7)),
                sps_1m: DecayingAverage::new(Duration::from_mins(1)),
                sps_5m: DecayingAverage::new(Duration::from_mins(5)),
                sps_15m: DecayingAverage::new(Duration::from_mins(15)),
                sps_1hr: DecayingAverage::new(Duration::from_hours(1)),
                best_ever: None,
                last_share: None,
                total_work: 0.0,
            }),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
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
        let mut stats = self.stats.lock();
        let diff = pool_diff.as_f64();
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
        drop(stats);
        self.accepted.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_rejected(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
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
        self.accepted.load(Ordering::Relaxed)
    }

    pub(crate) fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
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
