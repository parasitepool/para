use super::*;

#[derive(Clone)]
pub(crate) struct Stats {
    pub(crate) accepted_shares: u64,
    pub(crate) rejected_shares: u64,
    pub(crate) accepted_work: TotalWork,
    pub(crate) rejected_work: TotalWork,
    pub(crate) last_share: Option<Instant>,
    pub(crate) best_share: Option<Difficulty>,
    pub(crate) dsps_1m: DecayingAverage,
    pub(crate) dsps_5m: DecayingAverage,
    pub(crate) dsps_15m: DecayingAverage,
    pub(crate) dsps_1hr: DecayingAverage,
    pub(crate) dsps_6hr: DecayingAverage,
    pub(crate) dsps_1d: DecayingAverage,
    pub(crate) dsps_7d: DecayingAverage,
    pub(crate) sps_1m: DecayingAverage,
    pub(crate) sps_5m: DecayingAverage,
    pub(crate) sps_15m: DecayingAverage,
    pub(crate) sps_1hr: DecayingAverage,
}

impl Stats {
    pub(crate) fn new() -> Self {
        Self {
            accepted_shares: 0,
            rejected_shares: 0,
            accepted_work: TotalWork::ZERO,
            rejected_work: TotalWork::ZERO,
            last_share: None,
            best_share: None,
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
        }
    }

    pub(crate) fn absorb(&mut self, other: Stats, now: Instant) {
        self.accepted_shares += other.accepted_shares;
        self.rejected_shares += other.rejected_shares;
        self.accepted_work += other.accepted_work;
        self.rejected_work += other.rejected_work;
        self.dsps_1m.absorb(other.dsps_1m, now);
        self.dsps_5m.absorb(other.dsps_5m, now);
        self.dsps_15m.absorb(other.dsps_15m, now);
        self.dsps_1hr.absorb(other.dsps_1hr, now);
        self.dsps_6hr.absorb(other.dsps_6hr, now);
        self.dsps_1d.absorb(other.dsps_1d, now);
        self.dsps_7d.absorb(other.dsps_7d, now);
        self.sps_1m.absorb(other.sps_1m, now);
        self.sps_5m.absorb(other.sps_5m, now);
        self.sps_15m.absorb(other.sps_15m, now);
        self.sps_1hr.absorb(other.sps_1hr, now);

        if other
            .best_share
            .is_some_and(|other_diff| self.best_share.is_none_or(|diff| other_diff > diff))
        {
            self.best_share = other.best_share;
        }

        if other
            .last_share
            .is_some_and(|other_last| self.last_share.is_none_or(|last| other_last > last))
        {
            self.last_share = other.last_share;
        }
    }

    pub(crate) fn hashrate_1m(&self, now: Instant) -> HashRate {
        HashRate::from_dsps(self.dsps_1m.value_at(now))
    }

    pub(crate) fn hashrate_5m(&self, now: Instant) -> HashRate {
        HashRate::from_dsps(self.dsps_5m.value_at(now))
    }

    pub(crate) fn hashrate_15m(&self, now: Instant) -> HashRate {
        HashRate::from_dsps(self.dsps_15m.value_at(now))
    }

    pub(crate) fn hashrate_1hr(&self, now: Instant) -> HashRate {
        HashRate::from_dsps(self.dsps_1hr.value_at(now))
    }

    pub(crate) fn hashrate_6hr(&self, now: Instant) -> HashRate {
        HashRate::from_dsps(self.dsps_6hr.value_at(now))
    }

    pub(crate) fn hashrate_1d(&self, now: Instant) -> HashRate {
        HashRate::from_dsps(self.dsps_1d.value_at(now))
    }

    pub(crate) fn hashrate_7d(&self, now: Instant) -> HashRate {
        HashRate::from_dsps(self.dsps_7d.value_at(now))
    }

    pub(crate) fn sps_1m(&self, now: Instant) -> f64 {
        self.sps_1m.value_at(now)
    }

    pub(crate) fn sps_5m(&self, now: Instant) -> f64 {
        self.sps_5m.value_at(now)
    }

    pub(crate) fn sps_15m(&self, now: Instant) -> f64 {
        self.sps_15m.value_at(now)
    }

    pub(crate) fn sps_1hr(&self, now: Instant) -> f64 {
        self.sps_1hr.value_at(now)
    }
}
