use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, DeserializeFromStr, SerializeDisplay, Hash)]
#[repr(transparent)]
pub struct JobId(u64);

impl JobId {
    pub fn new(n: u64) -> Self {
        Self(n)
    }

    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

impl FromStr for JobId {
    type Err = InternalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let id = u64::from_str_radix(s, 16).map_err(|e| InternalError::Parse {
            message: format!("invalid job id hex string '{}': {}", s, e),
        })?;
        Ok(JobId(id))
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

impl From<JobId> for u64 {
    fn from(id: JobId) -> u64 {
        id.0
    }
}

impl From<u64> for JobId {
    fn from(id: u64) -> JobId {
        JobId(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobid_roundtrip() {
        assert_eq!(JobId::from(0).to_string(), "0");
        assert_eq!(JobId::from_str("0").unwrap(), JobId::from(0));

        assert_eq!(JobId::from(0x1fu64).to_string(), "1f");
        assert_eq!(JobId::from_str("1F").unwrap(), JobId::from(0x1f));

        assert_eq!(JobId::from(u64::MAX).to_string(), "ffffffffffffffff");
        assert_eq!(
            JobId::from_str("ffffffffffffffff").unwrap(),
            JobId::from(u64::MAX)
        );
    }

    #[test]
    fn jobid_errors() {
        assert!("".parse::<JobId>().is_err());
        assert!(" ".parse::<JobId>().is_err());
        assert!("0x1".parse::<JobId>().is_err());
        assert!("g".parse::<JobId>().is_err());
        assert!("10000000000000000".parse::<JobId>().is_err());
    }

    #[test]
    fn jobid_serde_json() {
        let id = JobId::from(0xdead_beefu64);
        let s = serde_json::to_string(&id).unwrap();
        assert_eq!(s, "\"deadbeef\"");
        let back: JobId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn jobid_wraps() {
        let job_id = JobId::new(u64::MAX - 1);
        assert_eq!(job_id.next(), JobId::new(u64::MAX));
        assert_eq!(job_id.next().next(), JobId::new(0));
    }
}
