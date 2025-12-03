use super::*;

pub const HASHES_PER_DIFF_1: u64 = 1 << 32; // 4,294,967,296
pub const HASHES_PER_DIFF_1_PRECISE: f64 = 4_295_032_833.0;

/// SI unit prefixes for hash rate display
const SI_PREFIXES: &[(&str, f64)] = &[
    ("", 1.0),
    ("K", 1e3),
    ("M", 1e6),
    ("G", 1e9),
    ("T", 1e12),
    ("P", 1e15),
    ("E", 1e18),
    ("Z", 1e21),
    ("Y", 1e24),
];

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct HashRate(pub f64);

impl HashRate {
    pub const ZERO: Self = Self(0.0);

    pub fn from_difficulty_rate(difficulty: f64, shares_per_sec: f64) -> Self {
        Self(difficulty * shares_per_sec * HASHES_PER_DIFF_1 as f64)
    }

    pub fn estimate(total_difficulty: f64, window: Duration) -> Self {
        if window.is_zero() {
            return Self::ZERO;
        }

        Self(total_difficulty * HASHES_PER_DIFF_1 as f64 / window.as_secs_f64())
    }

    /// Calculate relative standard deviation of the hash rate estimate.
    ///
    /// For a Stratum pool with target share period `p` and observation window `T`:
    ///
    /// ```text
    /// RSD = √(p / T)
    /// ```
    ///
    /// Returns the coefficient of variation (standard deviation / mean).
    ///
    /// # Examples
    ///
    /// - 5s period, 60s window → ~29% RSD
    /// - 5s period, 600s window → ~9% RSD
    /// - 5s period, 3600s window → ~3.7% RSD
    pub fn estimation_rsd(share_period: Duration, observation_window: Duration) -> f64 {
        if observation_window.is_zero() {
            return f64::INFINITY;
        }
        (share_period.as_secs_f64() / observation_window.as_secs_f64()).sqrt()
    }

    /// Convert to accumulated work over a duration.
    pub fn work_over(self, duration: Duration) -> Work {
        Work::from_hashes(self.0 * duration.as_secs_f64())
    }

    /// Returns true if the hash rate is zero
    pub fn is_zero(self) -> bool {
        self.0 == 0.0
    }
}

impl Display for HashRate {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        format_with_si_suffix(self.0, "H/s", f)
    }
}

impl FromStr for HashRate {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(parse_with_si_prefix(
            s,
            &["H/s", "h/s", "H", "h", ""],
        )?))
    }
}

impl Serialize for HashRate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Serialize as a compact string for JSON readability
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for HashRate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl Add for HashRate {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for HashRate {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for HashRate {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self((self.0 - rhs.0).max(0.0))
    }
}

impl SubAssign for HashRate {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 = (self.0 - rhs.0).max(0.0);
    }
}

impl Mul<f64> for HashRate {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        Self((self.0 * rhs).max(0.0))
    }
}

impl Div<f64> for HashRate {
    type Output = Self;
    fn div(self, rhs: f64) -> Self {
        if rhs == 0.0 {
            Self::ZERO
        } else {
            Self((self.0 / rhs).max(0.0))
        }
    }
}

/// Accumulated work: total expected hashes.
///
/// This is the canonical accumulated measure for mining—it's additive across
/// time and miners, making it suitable for:
///
/// - Reward calculations (FPPS/PPLNS)
/// - Long-term hashrate estimation
/// - Distributed consensus
///
/// # Two Representations
///
/// Work can be expressed in two ways:
///
/// 1. **Hashes**: Raw expected hash count (`W_h`)
/// 2. **Difficulty units**: Sum of share difficulties (`W_d = W_h / 2³²`)
///
/// The relationship is: `W_h = W_d × 2³²`
///
/// # Display Format
///
/// By default displays in hashes with SI prefix. Use `{:#}` for difficulty units.
///
/// ```
/// use para::hash_rate::Work;
///
/// let work = Work::from_difficulty(1000.0);
/// assert_eq!(format!("{}", work), "4.29 TH");
/// assert_eq!(format!("{:#}", work), "1000 diff");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Work(f64); // Stored as total hashes

impl Work {
    /// Zero work
    pub const ZERO: Self = Self(0.0);

    /// Create from total hash count
    pub fn from_hashes(hashes: f64) -> Self {
        Self(hashes.max(0.0))
    }

    /// Create from cumulative difficulty (sum of share difficulties)
    ///
    /// Each share of difficulty `d` contributes `d × 2³²` expected hashes.
    pub fn from_difficulty(difficulty: f64) -> Self {
        Self::from_hashes(difficulty * HASHES_PER_DIFF_1 as f64)
    }

    /// Get total hash count
    pub fn as_hashes(self) -> f64 {
        self.0
    }

    /// Get equivalent cumulative difficulty
    ///
    /// This is `W / 2³²`, the sum of share difficulties that would produce this work.
    pub fn as_difficulty(self) -> f64 {
        self.0 / HASHES_PER_DIFF_1 as f64
    }

    /// Returns true if the work is zero
    pub fn is_zero(self) -> bool {
        self.0 == 0.0
    }

    /// Calculate the implied hash rate over a duration
    pub fn hash_rate_over(self, duration: Duration) -> HashRate {
        if duration.is_zero() {
            return HashRate::ZERO;
        }
        HashRate(self.0 / duration.as_secs_f64())
    }

    /// Add work from a single share submission
    pub fn add_share(&mut self, difficulty: f64) {
        self.0 += difficulty * HASHES_PER_DIFF_1 as f64;
    }
}

impl Display for Work {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            // {:#} format: display as difficulty units
            format_number_with_suffix(self.as_difficulty(), "diff", f)
        } else {
            // Default: display as hashes
            format_with_si_suffix(self.0, "H", f)
        }
    }
}

impl FromStr for Work {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();

        // Check for difficulty unit suffix
        if let Some(num_str) = s.strip_suffix("diff").or_else(|| s.strip_suffix("d")) {
            let num = parse_with_si_prefix(num_str.trim(), &[""])?;
            return Ok(Self::from_difficulty(num));
        }

        // Otherwise parse as hashes
        parse_with_si_prefix(s, &["H", "h", ""]).map(Self::from_hashes)
    }
}

impl Serialize for Work {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Serialize as difficulty for compactness (most common use case)
        serializer.serialize_f64(self.as_difficulty())
    }
}

impl<'de> Deserialize<'de> for Work {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Deserialize from difficulty value
        let diff = f64::deserialize(deserializer)?;
        Ok(Self::from_difficulty(diff))
    }
}

impl Add for Work {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for Work {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for Work {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self((self.0 - rhs.0).max(0.0))
    }
}

impl SubAssign for Work {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 = (self.0 - rhs.0).max(0.0);
    }
}

impl Mul<f64> for Work {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        Self((self.0 * rhs).max(0.0))
    }
}

impl Div<f64> for Work {
    type Output = Self;
    fn div(self, rhs: f64) -> Self {
        if rhs == 0.0 {
            Self::ZERO
        } else {
            Self((self.0 / rhs).max(0.0))
        }
    }
}

/// Variance statistics for hashrate estimation
#[derive(Debug, Clone, Copy)]
pub struct EstimationVariance {
    /// Expected hashrate (mean of the estimator)
    pub expected: HashRate,
    /// Variance of the hashrate estimator
    pub variance: f64,
    /// Standard deviation
    pub std_dev: f64,
    /// Coefficient of variation (relative standard deviation)
    pub cv: f64,
}

impl EstimationVariance {
    /// Calculate variance statistics for a hashrate estimate.
    ///
    /// # Parameters
    ///
    /// - `hash_rate`: The estimated hash rate
    /// - `difficulty`: Share difficulty
    /// - `observation_window`: Duration over which shares were observed
    ///
    /// # Formula
    ///
    /// For Poisson share arrivals:
    /// ```text
    /// Var(Ĥ) = H × d / T
    /// ```
    pub fn calculate(hash_rate: HashRate, difficulty: f64, observation_window: Duration) -> Self {
        let h = hash_rate.0;
        let t = observation_window.as_secs_f64();

        if t == 0.0 || h == 0.0 {
            return Self {
                expected: hash_rate,
                variance: 0.0,
                std_dev: 0.0,
                cv: 0.0,
            };
        }

        let variance = h * difficulty * HASHES_PER_DIFF_1 as f64 / t;
        let std_dev = variance.sqrt();
        let cv = std_dev / h;

        Self {
            expected: hash_rate,
            variance,
            std_dev,
            cv,
        }
    }

    /// Create a 95% confidence interval for the hashrate
    pub fn confidence_interval_95(&self) -> (HashRate, HashRate) {
        let margin = 1.96 * self.std_dev;
        (
            HashRate((self.expected.0 - margin).max(0.0)),
            HashRate(self.expected.0 + margin),
        )
    }
}

// ============================================================================
// Hash Rate Estimation
// ============================================================================

/// Trait for types that can produce a hash rate estimate.
///
/// This allows different estimation strategies (EMA, simple average, etc.)
/// to be used interchangeably.
pub trait EstimateHashRate {
    /// Get the current hash rate estimate.
    fn hash_rate(&self) -> HashRate;

    /// Get the difficulty-weighted rate (difficulty per second).
    ///
    /// This is the raw value before conversion to hashes.
    fn difficulty_per_sec(&self) -> f64 {
        self.hash_rate().0 / HASHES_PER_DIFF_1 as f64
    }
}

/// EMA-based hash rate estimator from share submissions.
///
/// This wraps a [`DecayingAverage`] to track difficulty-weighted shares per second,
/// then converts to hash rate using the `2³²` constant.
///
/// # Example
///
/// ```ignore
/// use para::hash_rate::HashRateEstimator;
/// use std::time::Duration;
///
/// let mut estimator = HashRateEstimator::new(Duration::from_secs(300));
///
/// // Miner submits a share at difficulty 100
/// estimator.record_share(100.0);
///
/// // Get current hashrate estimate
/// println!("{}", estimator.hash_rate()); // e.g., "429 GH/s"
/// ```
#[derive(Debug, Clone)]
pub struct HashRateEstimator {
    /// Tracks difficulty-weighted shares per second (dsps)
    dsps: DecayingAverage,
    /// Total accumulated work (for long-term tracking)
    total_work: Work,
    /// Number of shares recorded
    share_count: u64,
    /// First share timestamp (for bias correction)
    first_share: Option<Instant>,
}

impl HashRateEstimator {
    /// Create a new estimator with the given EMA window.
    ///
    /// The window determines how quickly the estimate responds to changes:
    /// - Shorter window (e.g., 60s): More responsive, more jitter
    /// - Longer window (e.g., 300s): More stable, slower to respond
    ///
    /// A 5-minute (300s) window is typical for pool hashrate display.
    pub fn new(window: Duration) -> Self {
        Self {
            dsps: DecayingAverage::new(window),
            total_work: Work::ZERO,
            share_count: 0,
            first_share: None,
        }
    }

    /// Record a share submission at the given difficulty.
    ///
    /// Call this each time a miner submits a valid share.
    pub fn record_share(&mut self, difficulty: f64) {
        self.record_share_at(difficulty, Instant::now());
    }

    /// Record a share submission with an explicit timestamp.
    ///
    /// Useful for testing or replaying historical data.
    pub fn record_share_at(&mut self, difficulty: f64, now: Instant) {
        if self.first_share.is_none() {
            self.first_share = Some(now);
        }

        self.dsps.record(difficulty, now);
        self.total_work.add_share(difficulty);
        self.share_count += 1;
    }

    /// Get the difficulty-weighted shares per second (dsps).
    ///
    /// This is the raw EMA value before conversion to hash rate.
    pub fn dsps(&self) -> f64 {
        self.dsps.value()
    }

    /// Get the total accumulated work since creation.
    pub fn total_work(&self) -> Work {
        self.total_work
    }

    /// Get the number of shares recorded.
    pub fn share_count(&self) -> u64 {
        self.share_count
    }

    /// Get the time since the first share, if any.
    pub fn uptime(&self) -> Option<Duration> {
        self.first_share.map(|t| t.elapsed())
    }

    /// Calculate the average hashrate over the entire lifetime.
    ///
    /// This is more stable than the EMA for very long periods,
    /// but doesn't reflect recent changes.
    pub fn lifetime_average(&self) -> HashRate {
        match self.uptime() {
            Some(duration) if !duration.is_zero() => self.total_work.hash_rate_over(duration),
            _ => HashRate::ZERO,
        }
    }

    /// Reset the estimator to initial state.
    pub fn reset(&mut self) {
        let window = self.dsps.window();
        self.dsps = DecayingAverage::new(window);
        self.total_work = Work::ZERO;
        self.share_count = 0;
        self.first_share = None;
    }
}

impl EstimateHashRate for HashRateEstimator {
    fn hash_rate(&self) -> HashRate {
        HashRate(self.dsps.value() * HASHES_PER_DIFF_1 as f64)
    }

    fn difficulty_per_sec(&self) -> f64 {
        self.dsps.value()
    }
}

impl Default for HashRateEstimator {
    fn default() -> Self {
        // 5-minute window is standard for pool displays
        Self::new(Duration::from_secs(300))
    }
}

/// A collection of hashrate estimators at different time windows.
///
/// Pools typically display hashrate at multiple granularities
/// (1m, 5m, 15m, 1hr, etc.). This tracks all of them efficiently.
#[derive(Debug, Clone)]
pub struct MultiWindowEstimator {
    estimators: Vec<(Duration, HashRateEstimator)>,
    total_work: Work,
    share_count: u64,
}

impl MultiWindowEstimator {
    /// Create with the standard pool windows: 1m, 5m, 15m, 1hr, 6hr, 1d.
    pub fn standard() -> Self {
        Self::new(&[
            Duration::from_secs(60),    // 1 minute
            Duration::from_secs(300),   // 5 minutes
            Duration::from_secs(900),   // 15 minutes
            Duration::from_secs(3600),  // 1 hour
            Duration::from_secs(21600), // 6 hours
            Duration::from_secs(86400), // 1 day
        ])
    }

    /// Create with custom windows.
    pub fn new(windows: &[Duration]) -> Self {
        Self {
            estimators: windows
                .iter()
                .map(|&w| (w, HashRateEstimator::new(w)))
                .collect(),
            total_work: Work::ZERO,
            share_count: 0,
        }
    }

    /// Record a share across all windows.
    pub fn record_share(&mut self, difficulty: f64) {
        let now = Instant::now();
        for (_, est) in &mut self.estimators {
            est.record_share_at(difficulty, now);
        }
        self.total_work.add_share(difficulty);
        self.share_count += 1;
    }

    /// Get the hashrate at a specific window.
    ///
    /// Returns `None` if no estimator exists for that window.
    pub fn hash_rate_at(&self, window: Duration) -> Option<HashRate> {
        self.estimators
            .iter()
            .find(|(w, _)| *w == window)
            .map(|(_, est)| est.hash_rate())
    }

    /// Get all hashrates as (window, rate) pairs.
    pub fn all_rates(&self) -> Vec<(Duration, HashRate)> {
        self.estimators
            .iter()
            .map(|(w, est)| (*w, est.hash_rate()))
            .collect()
    }

    /// Get total work across all time.
    pub fn total_work(&self) -> Work {
        self.total_work
    }

    /// Get total share count.
    pub fn share_count(&self) -> u64 {
        self.share_count
    }
}

/// Error type for parsing hash rate and work values
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
}

impl Display for ParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ParseError {}

// Helper: format a number with SI prefix and unit suffix
fn format_with_si_suffix(value: f64, unit: &str, f: &mut Formatter<'_>) -> fmt::Result {
    if value == 0.0 {
        return write!(f, "0 {unit}");
    }

    let (prefix, divisor) = SI_PREFIXES
        .iter()
        .rev()
        .find(|(_, div)| value.abs() >= *div * 0.9999)
        .unwrap_or(&SI_PREFIXES[0]);

    let scaled = value / divisor;

    // Use 3 significant figures
    if scaled >= 100.0 {
        write!(f, "{:.0} {prefix}{unit}", scaled)
    } else if scaled >= 10.0 {
        write!(f, "{:.1} {prefix}{unit}", scaled)
    } else {
        write!(f, "{:.2} {prefix}{unit}", scaled)
    }
}

// Helper: format a number with optional SI prefix and suffix
fn format_number_with_suffix(value: f64, suffix: &str, f: &mut Formatter<'_>) -> fmt::Result {
    if value == 0.0 {
        return write!(f, "0 {suffix}");
    }

    let (prefix, divisor) = SI_PREFIXES
        .iter()
        .rev()
        .find(|(_, div)| value.abs() >= *div * 0.9999)
        .unwrap_or(&SI_PREFIXES[0]);

    let scaled = value / divisor;

    if prefix.is_empty() {
        if scaled >= 100.0 {
            write!(f, "{:.0} {suffix}", scaled)
        } else if scaled >= 10.0 {
            write!(f, "{:.1} {suffix}", scaled)
        } else {
            write!(f, "{:.2} {suffix}", scaled)
        }
    } else if scaled >= 100.0 {
        write!(f, "{:.0} {prefix}{suffix}", scaled)
    } else if scaled >= 10.0 {
        write!(f, "{:.1} {prefix}{suffix}", scaled)
    } else {
        write!(f, "{:.2} {prefix}{suffix}", scaled)
    }
}

// Helper: parse a number with SI prefix
fn parse_with_si_prefix(s: &str, valid_suffixes: &[&str]) -> Result<f64, ParseError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(ParseError {
            message: "empty string".to_string(),
        });
    }

    // Find and strip any valid suffix
    let mut num_part = s;
    for suffix in valid_suffixes {
        if let Some(stripped) = s.strip_suffix(suffix) {
            num_part = stripped.trim();
            break;
        }
    }

    // Check for SI prefix at the end
    let (num_str, multiplier) = if let Some(last_char) = num_part.chars().last() {
        let upper = last_char.to_ascii_uppercase();
        if let Some((_, mult)) = SI_PREFIXES
            .iter()
            .find(|(p, _)| !p.is_empty() && p.chars().next().unwrap().to_ascii_uppercase() == upper)
        {
            (&num_part[..num_part.len() - last_char.len_utf8()], *mult)
        } else if last_char.is_ascii_digit() || last_char == '.' {
            (num_part, 1.0)
        } else {
            return Err(ParseError {
                message: format!("invalid suffix: {last_char}"),
            });
        }
    } else {
        (num_part, 1.0)
    };

    let num_str = num_str.trim();
    let num: f64 = num_str.parse().map_err(|e| ParseError {
        message: format!("invalid number: {e}"),
    })?;

    if !num.is_finite() || num < 0.0 {
        return Err(ParseError {
            message: "value must be finite and non-negative".to_string(),
        });
    }

    Ok(num * multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== Constants Tests ==========

    #[test]
    fn hashes_per_diff_1_is_2_to_32() {
        assert_eq!(HASHES_PER_DIFF_1, 4_294_967_296);
        assert_eq!(HASHES_PER_DIFF_1, 1u64 << 32);
    }

    #[test]
    fn precise_constant_close_to_power_of_2() {
        let ratio = HASHES_PER_DIFF_1_PRECISE / HASHES_PER_DIFF_1 as f64;
        assert!((ratio - 1.0).abs() < 0.00002, "ratio: {ratio}");
    }

    // ========== HashRate Tests ==========

    #[test]
    fn hashrate_from_hashes_per_sec() {
        let rate = HashRate(1e12);
        assert_eq!(rate.0, 1e12);
    }

    #[test]
    fn hashrate_from_difficulty_rate() {
        // 1 share/sec at difficulty 1 → 2³² H/s
        let rate = HashRate::from_difficulty_rate(1.0, 1.0);
        assert_eq!(rate.0, HASHES_PER_DIFF_1 as f64);

        // 0.2 shares/sec at difficulty 100 → 20 × 2³² H/s
        let rate = HashRate::from_difficulty_rate(100.0, 0.2);
        assert_eq!(rate.0, 20.0 * HASHES_PER_DIFF_1 as f64);
    }

    #[test]
    fn hashrate_estimate() {
        // 60 total difficulty over 60 seconds → 1 diff/s → 2³² H/s
        let rate = HashRate::estimate(60.0, Duration::from_secs(60));
        assert_eq!(rate.0, HASHES_PER_DIFF_1 as f64);
    }

    #[test]
    fn hashrate_estimate_zero_window() {
        let rate = HashRate::estimate(100.0, Duration::ZERO);
        assert!(rate.is_zero());
    }

    #[test]
    fn hashrate_display_formatting() {
        let cases = [
            (0.0, "0 H/s"),
            (1e3, "1.00 KH/s"),
            (1e6, "1.00 MH/s"),
            (1e9, "1.00 GH/s"),
            (1e12, "1.00 TH/s"),
            (1e15, "1.00 PH/s"),
            (1e18, "1.00 EH/s"),
            (314e15, "314 PH/s"),
            (1.5e12, "1.50 TH/s"),
            (45.6e12, "45.6 TH/s"),
            (456e12, "456 TH/s"),
        ];

        for (value, expected) in cases {
            let rate = HashRate(value);
            assert_eq!(rate.to_string(), expected, "for value {value}");
        }
    }

    #[test]
    fn hashrate_parse() {
        let cases = [
            ("0", 0.0),
            ("0 H/s", 0.0),
            ("1K", 1e3),
            ("1 KH/s", 1e3),
            ("1.5M", 1.5e6),
            ("1.5 MH/s", 1.5e6),
            ("100G", 1e11),
            ("100 GH/s", 1e11),
            ("1T", 1e12),
            ("1 TH/s", 1e12),
            ("314P", 314e15),
            ("314 PH/s", 314e15),
            ("1E", 1e18),
            ("1 EH/s", 1e18),
        ];

        for (input, expected) in cases {
            let rate: HashRate = input.parse().unwrap();
            let actual = rate.0;
            let rel_err = if expected == 0.0 {
                actual
            } else {
                ((actual - expected) / expected).abs()
            };
            assert!(
                rel_err < 1e-10,
                "parse({input}): got {actual}, want {expected}"
            );
        }
    }

    #[test]
    fn hashrate_parse_errors() {
        let invalid = ["", "abc", "-1", "NaN", "Infinity"];
        for input in invalid {
            assert!(input.parse::<HashRate>().is_err(), "should reject: {input}");
        }
    }

    #[test]
    fn hashrate_arithmetic() {
        let a = HashRate(1e12);
        let b = HashRate(2e12);

        assert_eq!((a + b).0, 3e12);
        assert_eq!((b - a).0, 1e12);
        assert_eq!((a * 2.0).0, 2e12);
        assert_eq!((b / 2.0).0, 1e12);
    }

    #[test]
    fn hashrate_subtraction_clamps() {
        let a = HashRate(1e12);
        let b = HashRate(2e12);
        assert_eq!((a - b).0, 0.0);
    }

    #[test]
    fn hashrate_serde_roundtrip() {
        let rate = HashRate(1.5e12);
        let json = serde_json::to_string(&rate).unwrap();
        let parsed: HashRate = serde_json::from_str(&json).unwrap();
        assert_eq!(rate, parsed);
    }

    // ========== RSD Tests ==========

    #[test]
    fn rsd_5_sec_period() {
        // For 5s share period, RSD(60s) ≈ 28.8%
        let rsd = HashRate::estimation_rsd(Duration::from_secs(5), Duration::from_secs(60));
        assert!((rsd - 0.2887).abs() < 0.001, "RSD(60s): {rsd}");

        // RSD(600s) ≈ 9.1%
        let rsd = HashRate::estimation_rsd(Duration::from_secs(5), Duration::from_secs(600));
        assert!((rsd - 0.0913).abs() < 0.001, "RSD(600s): {rsd}");

        // RSD(3600s) ≈ 3.7%
        let rsd = HashRate::estimation_rsd(Duration::from_secs(5), Duration::from_secs(3600));
        assert!((rsd - 0.0373).abs() < 0.001, "RSD(3600s): {rsd}");
    }

    #[test]
    fn rsd_zero_window_is_infinite() {
        let rsd = HashRate::estimation_rsd(Duration::from_secs(5), Duration::ZERO);
        assert!(rsd.is_infinite());
    }

    // ========== Work Tests ==========

    #[test]
    fn work_from_hashes() {
        let work = Work::from_hashes(1e15);
        assert_eq!(work.as_hashes(), 1e15);
    }

    #[test]
    fn work_from_difficulty() {
        // 1 difficulty = 2³² hashes
        let work = Work::from_difficulty(1.0);
        assert_eq!(work.as_hashes(), HASHES_PER_DIFF_1 as f64);

        // 1000 difficulty = 1000 × 2³² hashes
        let work = Work::from_difficulty(1000.0);
        assert_eq!(work.as_hashes(), 1000.0 * HASHES_PER_DIFF_1 as f64);
    }

    #[test]
    fn work_as_difficulty() {
        let work = Work::from_hashes(HASHES_PER_DIFF_1 as f64 * 42.0);
        assert_eq!(work.as_difficulty(), 42.0);
    }

    #[test]
    fn work_display_default() {
        let work = Work::from_difficulty(1000.0);
        // 1000 × 2³² = 4.29 × 10¹² = 4.29 TH
        let s = work.to_string();
        assert!(s.contains("TH"), "expected TH: {s}");
    }

    #[test]
    fn work_display_alternate() {
        let work = Work::from_difficulty(1000.0);
        let s = format!("{:#}", work);
        assert!(
            s.contains("1000") || s.contains("1.00 K"),
            "expected ~1000 diff: {s}"
        );
        assert!(s.contains("diff"), "expected diff suffix: {s}");
    }

    #[test]
    fn work_parse_hashes() {
        let work: Work = "1 TH".parse().unwrap();
        assert_eq!(work.as_hashes(), 1e12);

        // 2³² ≈ 4.295 × 10⁹, so 4.295 TH ≈ 1000 difficulty
        let work: Work = "4.295 TH".parse().unwrap();
        let expected_diff = 4.295e12 / HASHES_PER_DIFF_1 as f64;
        assert!((work.as_difficulty() - expected_diff).abs() < 0.01);
    }

    #[test]
    fn work_parse_difficulty() {
        let work: Work = "1000 diff".parse().unwrap();
        assert_eq!(work.as_difficulty(), 1000.0);

        let work: Work = "1K diff".parse().unwrap();
        assert_eq!(work.as_difficulty(), 1000.0);
    }

    #[test]
    fn work_arithmetic() {
        let a = Work::from_difficulty(100.0);
        let b = Work::from_difficulty(200.0);

        assert_eq!((a + b).as_difficulty(), 300.0);
        assert_eq!((b - a).as_difficulty(), 100.0);
        assert_eq!((a * 3.0).as_difficulty(), 300.0);
        assert_eq!((b / 2.0).as_difficulty(), 100.0);
    }

    #[test]
    fn work_add_share() {
        let mut work = Work::ZERO;
        work.add_share(10.0);
        work.add_share(20.0);
        assert_eq!(work.as_difficulty(), 30.0);
    }

    #[test]
    fn work_hash_rate_over() {
        let work = Work::from_difficulty(60.0);
        let rate = work.hash_rate_over(Duration::from_secs(60));
        // 60 diff / 60 sec = 1 diff/sec = 2³² H/s
        assert_eq!(rate.0, HASHES_PER_DIFF_1 as f64);
    }

    #[test]
    fn work_serde_roundtrip() {
        let work = Work::from_difficulty(1234.5);
        let json = serde_json::to_string(&work).unwrap();
        let parsed: Work = serde_json::from_str(&json).unwrap();
        assert!((work.as_difficulty() - parsed.as_difficulty()).abs() < 1e-10);
    }

    // ========== HashRate <-> Work Conversion ==========

    #[test]
    fn hashrate_work_conversion() {
        let rate = HashRate(1e12);
        let work = rate.work_over(Duration::from_secs(3600));
        // 1 TH/s × 3600s = 3.6 PH
        assert_eq!(work.as_hashes(), 3.6e15);

        let rate_back = work.hash_rate_over(Duration::from_secs(3600));
        assert_eq!(rate_back.0, 1e12);
    }

    // ========== EstimationVariance Tests ==========

    #[test]
    fn variance_calculation() {
        let rate = HashRate(1e12);
        let stats = EstimationVariance::calculate(rate, 100.0, Duration::from_secs(600));

        assert!(stats.variance > 0.0);
        assert!(stats.std_dev > 0.0);
        assert!(stats.cv > 0.0);
        assert!(stats.cv < 1.0); // CV should be reasonable for 10 minutes
    }

    #[test]
    fn confidence_interval() {
        let rate = HashRate(1e12);
        let stats = EstimationVariance::calculate(rate, 100.0, Duration::from_secs(3600));

        let (low, high) = stats.confidence_interval_95();
        assert!(low.0 < rate.0);
        assert!(high.0 > rate.0);
    }

    // ========== HashRateEstimator Tests ==========

    #[test]
    fn estimator_new_is_zero() {
        let est = HashRateEstimator::new(Duration::from_secs(60));
        assert!(est.hash_rate().is_zero());
        assert_eq!(est.share_count(), 0);
        assert!(est.total_work().is_zero());
    }

    #[test]
    fn estimator_records_shares() {
        let start = Instant::now();
        let mut est = HashRateEstimator::new(Duration::from_secs(60));

        // First share
        est.record_share_at(100.0, start);

        // Second share 1 second later
        est.record_share_at(100.0, start + Duration::from_secs(1));

        assert_eq!(est.share_count(), 2);
        assert_eq!(est.total_work().as_difficulty(), 200.0);

        // dsps should be positive after shares
        assert!(est.dsps() > 0.0);
    }

    #[test]
    fn estimator_hash_rate_from_dsps() {
        let start = Instant::now();
        let mut est = HashRateEstimator::new(Duration::from_secs(60));

        // Simulate steady 1 share/sec at difficulty 10
        for i in 1..=60 {
            est.record_share_at(10.0, start + Duration::from_secs(i));
        }

        // dsps should be approximately 10
        let dsps = est.dsps();
        assert!(dsps > 5.0 && dsps < 15.0, "dsps: {dsps}");

        // hash_rate = dsps × 2³²
        let rate = est.hash_rate();
        assert!(rate.0 > 0.0);
        assert!((rate.0 / HASHES_PER_DIFF_1 as f64 - dsps).abs() < 0.001);
    }

    #[test]
    fn estimator_trait_implementation() {
        let est = HashRateEstimator::default();

        // Verify it implements EstimateHashRate
        fn check_trait<T: EstimateHashRate>(t: &T) -> HashRate {
            t.hash_rate()
        }

        let rate = check_trait(&est);
        assert!(rate.is_zero());
    }

    #[test]
    fn estimator_lifetime_average() {
        let start = Instant::now();
        let mut est = HashRateEstimator::new(Duration::from_secs(60));

        // Record 100 difficulty over ~1 second
        est.record_share_at(50.0, start);
        est.record_share_at(50.0, start + Duration::from_millis(1000));

        let avg = est.lifetime_average();
        // Should be approximately 100 diff / 1 sec = 100 diff/s = 100 × 2³² H/s
        assert!(avg.0 > 50.0 * HASHES_PER_DIFF_1 as f64);
    }

    #[test]
    fn estimator_reset() {
        let mut est = HashRateEstimator::new(Duration::from_secs(60));
        est.record_share(100.0);
        est.record_share(100.0);

        assert_eq!(est.share_count(), 2);

        est.reset();

        assert_eq!(est.share_count(), 0);
        assert!(est.total_work().is_zero());
        assert!(est.hash_rate().is_zero());
    }

    // ========== MultiWindowEstimator Tests ==========

    #[test]
    fn multi_window_standard_has_6_windows() {
        let mw = MultiWindowEstimator::standard();
        assert_eq!(mw.all_rates().len(), 6);
    }

    #[test]
    fn multi_window_records_to_all() {
        let mut mw = MultiWindowEstimator::standard();
        mw.record_share(100.0);

        assert_eq!(mw.share_count(), 1);
        assert_eq!(mw.total_work().as_difficulty(), 100.0);

        // All windows should have recorded
        for (_, rate) in mw.all_rates() {
            // Rates might be 0 or positive depending on timing
            assert!(rate.0 >= 0.0);
        }
    }

    #[test]
    fn multi_window_hash_rate_at() {
        let mw = MultiWindowEstimator::standard();

        // Should find the 1-minute window
        let rate = mw.hash_rate_at(Duration::from_secs(60));
        assert!(rate.is_some());

        // Should not find a random window
        let rate = mw.hash_rate_at(Duration::from_secs(123));
        assert!(rate.is_none());
    }
}
