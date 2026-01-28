use {super::*, parking_lot::Mutex};

const MIN_1: Duration = Duration::from_secs(60);
const MIN_5: Duration = Duration::from_secs(300);
const MIN_15: Duration = Duration::from_secs(900);
const HOUR_1: Duration = Duration::from_secs(3600);
const HOUR_6: Duration = Duration::from_secs(21600);
const DAY_1: Duration = Duration::from_secs(86400);
const WEEK_1: Duration = Duration::from_secs(604800);

struct DspsAverages {
    m1: DecayingAverage,
    m5: DecayingAverage,
    m15: DecayingAverage,
    h1: DecayingAverage,
    h6: DecayingAverage,
    d1: DecayingAverage,
    d7: DecayingAverage,
}

impl DspsAverages {
    fn new() -> Self {
        Self {
            m1: DecayingAverage::new(MIN_1),
            m5: DecayingAverage::new(MIN_5),
            m15: DecayingAverage::new(MIN_15),
            h1: DecayingAverage::new(HOUR_1),
            h6: DecayingAverage::new(HOUR_6),
            d1: DecayingAverage::new(DAY_1),
            d7: DecayingAverage::new(WEEK_1),
        }
    }

    fn record(&mut self, diff: f64, now: Instant) {
        self.m1.record(diff, now);
        self.m5.record(diff, now);
        self.m15.record(diff, now);
        self.h1.record(diff, now);
        self.h6.record(diff, now);
        self.d1.record(diff, now);
        self.d7.record(diff, now);
    }
}

struct SpsTracking {
    m1: DecayingAverage,
    m5: DecayingAverage,
    m15: DecayingAverage,
    h1: DecayingAverage,
}

impl SpsTracking {
    fn new() -> Self {
        Self {
            m1: DecayingAverage::new(MIN_1),
            m5: DecayingAverage::new(MIN_5),
            m15: DecayingAverage::new(MIN_15),
            h1: DecayingAverage::new(HOUR_1),
        }
    }

    fn record(&mut self, now: Instant) {
        self.m1.record(1.0, now);
        self.m5.record(1.0, now);
        self.m15.record(1.0, now);
        self.h1.record(1.0, now);
    }
}

struct Stats {
    dsps: DspsAverages,
    sps: SpsTracking,
    best_ever: Option<Difficulty>,
    last_share: Option<Instant>,
    total_work: f64,
}

pub(crate) struct Worker {
    workername: String,
    stats: Mutex<Stats>,
    accepted: AtomicU64,
    rejected: AtomicU64,
}

impl Worker {
    pub(crate) fn new(workername: String) -> Self {
        Self {
            workername,
            stats: Mutex::new(Stats {
                dsps: DspsAverages::new(),
                sps: SpsTracking::new(),
                best_ever: None,
                last_share: None,
                total_work: 0.0,
            }),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }

    pub(crate) fn record_accepted(&self, pool_diff: Difficulty, share_diff: Difficulty) {
        let now = Instant::now();
        let mut stats = self.stats.lock();
        stats.dsps.record(pool_diff.as_f64(), now);
        stats.sps.record(now);
        stats.total_work += pool_diff.as_f64();
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

    pub(crate) fn workername(&self) -> &str {
        &self.workername
    }

    pub(crate) fn hashrate_1m(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps.m1.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_5m(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps.m5.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_15m(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps.m15.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_1hr(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps.h1.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_6hr(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps.h6.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_1d(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps.d1.value_at(Instant::now()))
    }

    pub(crate) fn hashrate_7d(&self) -> HashRate {
        HashRate::from_dsps(self.stats.lock().dsps.d7.value_at(Instant::now()))
    }

    pub(crate) fn sps_1m(&self) -> f64 {
        self.stats.lock().sps.m1.value_at(Instant::now())
    }

    pub(crate) fn sps_5m(&self) -> f64 {
        self.stats.lock().sps.m5.value_at(Instant::now())
    }

    pub(crate) fn sps_15m(&self) -> f64 {
        self.stats.lock().sps.m15.value_at(Instant::now())
    }

    pub(crate) fn sps_1hr(&self) -> f64 {
        self.stats.lock().sps.h1.value_at(Instant::now())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_worker_hashrates_start_at_zero() {
        let worker = Worker::new("foo".to_string());
        assert_eq!(worker.hashrate_1m(), HashRate::ZERO);
        assert_eq!(worker.hashrate_5m(), HashRate::ZERO);
        assert_eq!(worker.hashrate_15m(), HashRate::ZERO);
        assert_eq!(worker.hashrate_1hr(), HashRate::ZERO);
        assert_eq!(worker.hashrate_6hr(), HashRate::ZERO);
        assert_eq!(worker.hashrate_1d(), HashRate::ZERO);
        assert_eq!(worker.hashrate_7d(), HashRate::ZERO);
    }

    #[test]
    fn new_worker_sps_start_at_zero() {
        let worker = Worker::new("foo".to_string());
        assert_eq!(worker.sps_1m(), 0.0);
        assert_eq!(worker.sps_5m(), 0.0);
        assert_eq!(worker.sps_15m(), 0.0);
        assert_eq!(worker.sps_1hr(), 0.0);
    }

    #[test]
    fn record_accepted_updates_all_hashrate_timeframes() {
        let worker = Worker::new("foo".to_string());
        let pool_diff = Difficulty::from(1000.0);
        let share_diff = Difficulty::from(1500.0);

        worker.record_accepted(pool_diff, share_diff);

        assert!(worker.hashrate_1m() > HashRate::ZERO);
        assert!(worker.hashrate_5m() > HashRate::ZERO);
        assert!(worker.hashrate_15m() > HashRate::ZERO);
        assert!(worker.hashrate_1hr() > HashRate::ZERO);
        assert!(worker.hashrate_6hr() > HashRate::ZERO);
        assert!(worker.hashrate_1d() > HashRate::ZERO);
        assert!(worker.hashrate_7d() > HashRate::ZERO);
    }

    #[test]
    fn record_accepted_updates_all_sps_timeframes() {
        let worker = Worker::new("foo".to_string());
        let pool_diff = Difficulty::from(1000.0);
        let share_diff = Difficulty::from(1500.0);

        worker.record_accepted(pool_diff, share_diff);

        assert!(worker.sps_1m() > 0.0);
        assert!(worker.sps_5m() > 0.0);
        assert!(worker.sps_15m() > 0.0);
        assert!(worker.sps_1hr() > 0.0);
    }

    #[test]
    fn time_window_constants() {
        assert_eq!(MIN_1, Duration::from_secs(60));
        assert_eq!(MIN_5, Duration::from_secs(300));
        assert_eq!(MIN_15, Duration::from_secs(900));
        assert_eq!(HOUR_1, Duration::from_secs(3600));
        assert_eq!(HOUR_6, Duration::from_secs(21600));
        assert_eq!(DAY_1, Duration::from_secs(86400));
        assert_eq!(WEEK_1, Duration::from_secs(604800));
    }
}
