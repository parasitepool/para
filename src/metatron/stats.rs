use {
    super::*,
    crate::{
        epoch::{epoch_secs_to_instant, instant_to_epoch_secs},
        store::entry::StatsEntry,
    },
};

#[derive(Clone)]
pub(crate) struct Stats {
    pub(crate) accepted_shares: u64,
    pub(crate) rejected_shares: u64,
    pub(crate) accepted_work: HashWork,
    pub(crate) rejected_work: HashWork,
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

impl Default for Stats {
    fn default() -> Self {
        Self::new()
    }
}

impl Stats {
    pub(crate) fn to_entry(&self, now: Instant) -> StatsEntry {
        StatsEntry {
            accepted_shares: self.accepted_shares,
            rejected_shares: self.rejected_shares,
            accepted_work: self.accepted_work,
            rejected_work: self.rejected_work,
            last_share_secs: self
                .last_share
                .map(|last_share| instant_to_epoch_secs(last_share, now)),
            best_share: self.best_share,
            dsps_1m: self.dsps_1m.to_entry(now),
            dsps_5m: self.dsps_5m.to_entry(now),
            dsps_15m: self.dsps_15m.to_entry(now),
            dsps_1hr: self.dsps_1hr.to_entry(now),
            dsps_6hr: self.dsps_6hr.to_entry(now),
            dsps_1d: self.dsps_1d.to_entry(now),
            dsps_7d: self.dsps_7d.to_entry(now),
            sps_1m: self.sps_1m.to_entry(now),
            sps_5m: self.sps_5m.to_entry(now),
            sps_15m: self.sps_15m.to_entry(now),
            sps_1hr: self.sps_1hr.to_entry(now),
        }
    }

    pub(crate) fn from_entry(entry: StatsEntry) -> Result<Self> {
        let last_share = entry
            .last_share_secs
            .filter(|secs| secs.is_finite())
            .map(epoch_secs_to_instant);

        Ok(Self {
            accepted_shares: entry.accepted_shares,
            rejected_shares: entry.rejected_shares,
            accepted_work: entry.accepted_work,
            rejected_work: entry.rejected_work,
            last_share,
            best_share: entry.best_share,
            dsps_1m: DecayingAverage::from_entry(entry.dsps_1m)?,
            dsps_5m: DecayingAverage::from_entry(entry.dsps_5m)?,
            dsps_15m: DecayingAverage::from_entry(entry.dsps_15m)?,
            dsps_1hr: DecayingAverage::from_entry(entry.dsps_1hr)?,
            dsps_6hr: DecayingAverage::from_entry(entry.dsps_6hr)?,
            dsps_1d: DecayingAverage::from_entry(entry.dsps_1d)?,
            dsps_7d: DecayingAverage::from_entry(entry.dsps_7d)?,
            sps_1m: DecayingAverage::from_entry(entry.sps_1m)?,
            sps_5m: DecayingAverage::from_entry(entry.sps_5m)?,
            sps_15m: DecayingAverage::from_entry(entry.sps_15m)?,
            sps_1hr: DecayingAverage::from_entry(entry.sps_1hr)?,
        })
    }

    pub(crate) fn new() -> Self {
        Self {
            accepted_shares: 0,
            rejected_shares: 0,
            accepted_work: HashWork::ZERO,
            rejected_work: HashWork::ZERO,
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

    pub(crate) fn record_accepted(
        &mut self,
        pool_diff: Difficulty,
        share_diff: Difficulty,
        now: Instant,
    ) {
        let diff = pool_diff.as_f64();

        self.accepted_shares += 1;
        self.dsps_1m.record(diff, now);
        self.dsps_5m.record(diff, now);
        self.dsps_15m.record(diff, now);
        self.dsps_1hr.record(diff, now);
        self.dsps_6hr.record(diff, now);
        self.dsps_1d.record(diff, now);
        self.dsps_7d.record(diff, now);
        self.sps_1m.record(1.0, now);
        self.sps_5m.record(1.0, now);
        self.sps_15m.record(1.0, now);
        self.sps_1hr.record(1.0, now);
        self.accepted_work += HashWork::from_difficulty(pool_diff);
        self.last_share = Some(now);

        if self.best_share.is_none_or(|best| share_diff > best) {
            self.best_share = Some(share_diff);
        }
    }

    pub(crate) fn record_rejected(&mut self, pool_diff: Difficulty) {
        self.rejected_shares += 1;
        self.rejected_work += HashWork::from_difficulty(pool_diff);
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
