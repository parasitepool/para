use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct TotalWork(pub(crate) f64);

impl TotalWork {
    pub const ZERO: Self = Self(0.0);

    pub fn from_difficulty(difficulty: Difficulty) -> Self {
        Self(difficulty.as_f64())
    }

    pub fn as_f64(self) -> f64 {
        self.0
    }
}

impl Display for TotalWork {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        format_si(self.0, "", f)
    }
}

impl Add for TotalWork {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for TotalWork {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for TotalWork {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

impl SubAssign for TotalWork {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(TotalWork(0.0).to_string(), "0");
        assert_eq!(TotalWork(3_161_600_000.0).to_string(), "3.16G");
        assert_eq!(TotalWork(1e6).to_string(), "1M");
    }

    #[test]
    fn arithmetic() {
        let a = TotalWork(100.0);
        let b = TotalWork(200.0);
        assert_eq!((a + b).0, 300.0);
        assert_eq!((b - a).0, 100.0);

        let mut c = TotalWork::ZERO;
        c += a;
        c += b;
        assert_eq!(c.0, 300.0);
        c -= a;
        assert_eq!(c.0, 200.0);
    }

    #[test]
    fn serde_roundtrip() {
        let work = TotalWork(1234.5);
        let json = serde_json::to_string(&work).unwrap();
        assert_eq!(json, "1234.5");
        let parsed: TotalWork = serde_json::from_str(&json).unwrap();
        assert_eq!(work, parsed);
    }
}
