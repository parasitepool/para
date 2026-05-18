use {super::*, crate::epoch::duration_from_secs_ago};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OrderEntry {
    pub(crate) status: OrderStatus,
    pub(crate) upstream_target: UpstreamTarget,
    pub(crate) bucket: Option<BucketEntry>,
    pub(crate) created_at_secs: f64,
    pub(crate) stats: StatsEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BucketEntry {
    pub(crate) target: HashDays,
    pub(crate) address: Address<NetworkUnchecked>,
    pub(crate) derivation_index: u32,
    pub(crate) amount_sat: u64,
    pub(crate) created_at_height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StatsEntry {
    pub(crate) accepted_shares: u64,
    pub(crate) rejected_shares: u64,
    pub(crate) accepted_work: HashWork,
    pub(crate) rejected_work: HashWork,
    pub(crate) last_share_secs_ago: Option<f64>,
    pub(crate) best_share: Option<Difficulty>,
    pub(crate) dsps_1m: DecayEntry,
    pub(crate) dsps_5m: DecayEntry,
    pub(crate) dsps_15m: DecayEntry,
    pub(crate) dsps_1hr: DecayEntry,
    pub(crate) dsps_6hr: DecayEntry,
    pub(crate) dsps_1d: DecayEntry,
    pub(crate) dsps_7d: DecayEntry,
    pub(crate) sps_1m: DecayEntry,
    pub(crate) sps_5m: DecayEntry,
    pub(crate) sps_15m: DecayEntry,
    pub(crate) sps_1hr: DecayEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DecayEntry {
    pub(crate) value: f64,
    pub(crate) window_secs: f64,
    pub(crate) last_update_secs_ago: f64,
}

impl StatsEntry {
    pub(crate) fn from_stats(stats: &Stats, now: Instant) -> Self {
        Self {
            accepted_shares: stats.accepted_shares,
            rejected_shares: stats.rejected_shares,
            accepted_work: stats.accepted_work,
            rejected_work: stats.rejected_work,
            last_share_secs_ago: stats
                .last_share
                .map(|last_share| now.saturating_duration_since(last_share).as_secs_f64()),
            best_share: stats.best_share,
            dsps_1m: DecayEntry::from_average(&stats.dsps_1m, now),
            dsps_5m: DecayEntry::from_average(&stats.dsps_5m, now),
            dsps_15m: DecayEntry::from_average(&stats.dsps_15m, now),
            dsps_1hr: DecayEntry::from_average(&stats.dsps_1hr, now),
            dsps_6hr: DecayEntry::from_average(&stats.dsps_6hr, now),
            dsps_1d: DecayEntry::from_average(&stats.dsps_1d, now),
            dsps_7d: DecayEntry::from_average(&stats.dsps_7d, now),
            sps_1m: DecayEntry::from_average(&stats.sps_1m, now),
            sps_5m: DecayEntry::from_average(&stats.sps_5m, now),
            sps_15m: DecayEntry::from_average(&stats.sps_15m, now),
            sps_1hr: DecayEntry::from_average(&stats.sps_1hr, now),
        }
    }

    pub(crate) fn into_stats(self) -> Result<Stats> {
        let now = Instant::now();

        let last_share = self
            .last_share_secs_ago
            .map(|secs| duration_from_secs_ago(secs, "last_share_secs_ago"))
            .transpose()?
            .map(|duration| now.checked_sub(duration).unwrap_or(now));

        Ok(Stats {
            accepted_shares: self.accepted_shares,
            rejected_shares: self.rejected_shares,
            accepted_work: self.accepted_work,
            rejected_work: self.rejected_work,
            last_share,
            best_share: self.best_share,
            dsps_1m: self.dsps_1m.into_average(now, "dsps_1m")?,
            dsps_5m: self.dsps_5m.into_average(now, "dsps_5m")?,
            dsps_15m: self.dsps_15m.into_average(now, "dsps_15m")?,
            dsps_1hr: self.dsps_1hr.into_average(now, "dsps_1hr")?,
            dsps_6hr: self.dsps_6hr.into_average(now, "dsps_6hr")?,
            dsps_1d: self.dsps_1d.into_average(now, "dsps_1d")?,
            dsps_7d: self.dsps_7d.into_average(now, "dsps_7d")?,
            sps_1m: self.sps_1m.into_average(now, "sps_1m")?,
            sps_5m: self.sps_5m.into_average(now, "sps_5m")?,
            sps_15m: self.sps_15m.into_average(now, "sps_15m")?,
            sps_1hr: self.sps_1hr.into_average(now, "sps_1hr")?,
        })
    }
}

impl DecayEntry {
    pub(crate) fn from_average(average: &DecayingAverage, now: Instant) -> Self {
        Self {
            value: average.value(),
            window_secs: average.window().as_secs_f64(),
            last_update_secs_ago: now
                .saturating_duration_since(average.last_update())
                .as_secs_f64(),
        }
    }

    pub(crate) fn into_average(self, now: Instant, field: &str) -> Result<DecayingAverage> {
        ensure!(self.value.is_finite(), "{field}.value must be finite");
        ensure!(
            self.window_secs.is_finite() && self.window_secs > 0.0,
            "{field}.window_secs must be finite and positive",
        );
        let last_update = now
            .checked_sub(duration_from_secs_ago(
                self.last_update_secs_ago,
                &format!("{field}.last_update_secs_ago"),
            )?)
            .unwrap_or(now);

        Ok(DecayingAverage::restore(
            self.value,
            Duration::from_secs_f64(self.window_secs),
            last_update,
        ))
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::epoch::instant_to_epoch_secs};

    fn test_address() -> Address<NetworkUnchecked> {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse()
            .unwrap()
    }

    fn test_stats(now: Instant) -> Stats {
        let mut stats = Stats::new();
        stats.record_accepted(Difficulty::from(100.0), Difficulty::from(200.0), now);
        stats.record_rejected(Difficulty::from(50.0));
        stats
    }

    #[test]
    fn order_entry_cbor_round_trips() {
        let now = Instant::now();
        let entry = OrderEntry {
            status: OrderStatus::Active,
            upstream_target: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            bucket: Some(BucketEntry {
                target: HashDays::new(1.0).unwrap(),
                address: test_address(),
                derivation_index: 9,
                amount_sat: 1_000,
                created_at_height: 100,
            }),
            created_at_secs: instant_to_epoch_secs(now, now),
            stats: StatsEntry::from_stats(&test_stats(now), now),
        };

        let mut bytes = Vec::new();
        ciborium::into_writer(&entry, &mut bytes).unwrap();
        let decoded: OrderEntry = ciborium::from_reader(bytes.as_slice()).unwrap();

        assert_eq!(decoded.status, OrderStatus::Active);
        assert_eq!(decoded.bucket.unwrap().derivation_index, 9);
        assert_eq!(decoded.stats.accepted_shares, 1);
        assert_eq!(decoded.stats.rejected_shares, 1);
    }

    #[test]
    fn stats_entry_round_trip_preserves_values() {
        let now = Instant::now();
        let stats = test_stats(now);
        let restored = StatsEntry::from_stats(&stats, now).into_stats().unwrap();

        assert_eq!(restored.accepted_shares, stats.accepted_shares);
        assert_eq!(restored.rejected_shares, stats.rejected_shares);
        assert_eq!(restored.accepted_work, stats.accepted_work);
        assert_eq!(restored.rejected_work, stats.rejected_work);
        assert_eq!(restored.best_share, stats.best_share);
        assert!(restored.last_share.is_some());
    }

    #[test]
    fn decay_entry_restore_applies_elapsed_decay() {
        let average = DecayingAverage::restore(
            10.0,
            Duration::from_secs(60),
            Instant::now() - Duration::from_secs(60),
        );

        let restored = DecayEntry::from_average(&average, Instant::now())
            .into_average(Instant::now(), "test")
            .unwrap();

        let value = restored.value_at(Instant::now());
        assert!(value > 0.0);
        assert!(value < 10.0);
    }

    #[test]
    fn stats_entry_rejects_non_finite_durations() {
        let now = Instant::now();
        let mut entry = StatsEntry::from_stats(&test_stats(now), now);
        entry.dsps_1m.last_update_secs_ago = f64::NAN;

        assert!(entry.into_stats().is_err());
    }

    #[test]
    fn stats_entry_restore_handles_huge_secs_ago() {
        let now = Instant::now();
        let mut entry = StatsEntry::from_stats(&test_stats(now), now);
        entry.last_share_secs_ago = Some(1e30);
        entry.dsps_1m.last_update_secs_ago = 1e30;

        assert!(entry.into_stats().is_ok());
    }
}
