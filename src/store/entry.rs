use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OrderEntry {
    pub(crate) status: OrderStatus,
    #[serde(default)]
    pub(crate) review: Review,
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
    pub(crate) last_share_secs: Option<f64>,
    pub(crate) best_share: Option<Difficulty>,
    pub(crate) dsps_1m: DecayingAverageEntry,
    pub(crate) dsps_5m: DecayingAverageEntry,
    pub(crate) dsps_15m: DecayingAverageEntry,
    pub(crate) dsps_1hr: DecayingAverageEntry,
    pub(crate) dsps_6hr: DecayingAverageEntry,
    pub(crate) dsps_1d: DecayingAverageEntry,
    pub(crate) dsps_7d: DecayingAverageEntry,
    pub(crate) sps_1m: DecayingAverageEntry,
    pub(crate) sps_5m: DecayingAverageEntry,
    pub(crate) sps_15m: DecayingAverageEntry,
    pub(crate) sps_1hr: DecayingAverageEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DecayingAverageEntry {
    pub(crate) value: f64,
    pub(crate) window_secs: f64,
    pub(crate) last_update_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkerEntry {
    pub(crate) workername: String,
    pub(crate) stats: StatsEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UserEntry {
    pub(crate) authorized_secs: u64,
    pub(crate) workers: Vec<WorkerEntry>,
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
            review: Review::Clean,
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
            stats: test_stats(now).to_entry(now),
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
    fn order_entry_decodes_legacy_record_without_review() {
        #[derive(Serialize)]
        struct Legacy {
            status: OrderStatus,
            upstream_target: UpstreamTarget,
            bucket: Option<BucketEntry>,
            created_at_secs: f64,
            stats: StatsEntry,
        }

        let now = Instant::now();
        let legacy = Legacy {
            status: OrderStatus::Expired,
            upstream_target: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            bucket: None,
            created_at_secs: instant_to_epoch_secs(now, now),
            stats: test_stats(now).to_entry(now),
        };

        let mut bytes = Vec::new();
        ciborium::into_writer(&legacy, &mut bytes).unwrap();
        let decoded: OrderEntry = ciborium::from_reader(bytes.as_slice()).unwrap();

        assert_eq!(decoded.status, OrderStatus::Expired);
        assert_eq!(decoded.review, Review::Clean);
    }

    #[test]
    fn stats_entry_round_trip_preserves_values() {
        let now = Instant::now();
        let stats = test_stats(now);
        let restored = Stats::from_entry(stats.to_entry(now)).unwrap();

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

        let restored = DecayingAverage::from_entry(average.to_entry(Instant::now())).unwrap();

        let value = restored.value_at(Instant::now());
        assert!(value > 0.0);
        assert!(value < 10.0);
    }

    #[test]
    fn stats_entry_rejects_non_finite_durations() {
        let now = Instant::now();
        let mut entry = test_stats(now).to_entry(now);
        entry.dsps_1m.last_update_secs = f64::NAN;

        assert!(Stats::from_entry(entry).is_err());
    }

    #[test]
    fn stats_entry_restore_handles_huge_secs() {
        let now = Instant::now();
        let mut entry = test_stats(now).to_entry(now);
        entry.last_share_secs = Some(1e30);
        entry.dsps_1m.last_update_secs = 1e30;

        assert!(Stats::from_entry(entry).is_ok());
    }
}
