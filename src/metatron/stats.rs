use super::*;

pub(crate) struct Stats {
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
    pub(crate) best_ever: Option<Difficulty>,
    pub(crate) last_share: Option<Instant>,
    pub(crate) total_work: f64,
}
