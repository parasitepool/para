use super::*;

pub(super) struct Stats {
    pub(super) dsps_1m: DecayingAverage,
    pub(super) dsps_5m: DecayingAverage,
    pub(super) dsps_15m: DecayingAverage,
    pub(super) dsps_1hr: DecayingAverage,
    pub(super) dsps_6hr: DecayingAverage,
    pub(super) dsps_1d: DecayingAverage,
    pub(super) dsps_7d: DecayingAverage,
    pub(super) sps_1m: DecayingAverage,
    pub(super) sps_5m: DecayingAverage,
    pub(super) sps_15m: DecayingAverage,
    pub(super) sps_1hr: DecayingAverage,
    pub(super) best_ever: Option<Difficulty>,
    pub(super) last_share: Option<Instant>,
    pub(super) total_work: f64,
}
