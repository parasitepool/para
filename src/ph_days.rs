use {super::*, hashrate::HASHES_PER_DIFF_1};

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct PhDays(f64);

impl From<TotalWork> for PhDays {
    fn from(work: TotalWork) -> Self {
        Self(work.as_f64() * HASHES_PER_DIFF_1 as f64 / (1e15 * 86_400.0))
    }
}

impl Display for PhDays {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2} PHd", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion() {
        let work = TotalWork(86_400.0 * 1e15 / HASHES_PER_DIFF_1 as f64);
        let phd = PhDays::from(work);
        assert!((phd.0 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn display() {
        let phd = PhDays(0.84);
        assert_eq!(phd.to_string(), "0.84 PHd");
    }

    #[test]
    fn serde_roundtrip() {
        let phd = PhDays(1.5);
        let json = serde_json::to_string(&phd).unwrap();
        assert_eq!(json, "1.5");
        let parsed: PhDays = serde_json::from_str(&json).unwrap();
        assert_eq!(phd, parsed);
    }
}
