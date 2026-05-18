use super::*;

/// Expected hashes per difficulty-1 share: 2^32 =~ 4.29 billion.
/// The precise value is 2^256/target_1 =~ 4,295,032,833 (~0.0015% higher),
/// but 2^32 is the standard approximation used across the mining ecosystem.
pub(crate) const HASHES_PER_DIFF_1: u64 = 1 << 32;
pub(crate) const SECONDS_PER_DAY: f64 = 86_400.0;

pub(crate) fn saturating_finite(value: f64) -> f64 {
    if value.is_nan() || value <= 0.0 {
        0.0
    } else if value.is_finite() {
        value
    } else {
        f64::MAX
    }
}

pub mod days;
pub mod price;
pub mod rate;
pub mod value;
pub mod work;

pub use days::HashDays;
pub use price::HashPrice;
pub use rate::HashRate;
pub use value::HashValue;
pub use work::HashWork;
