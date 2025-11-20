use super::*;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, DeserializeFromStr, SerializeDisplay,
)]
pub struct Ntime(u32);

impl FromStr for Ntime {
    type Err = InternalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let time = u32::from_str_radix(s, 16).map_err(|e| InternalError::Parse {
            message: format!("invalid ntime hex string '{}': {}", s, e),
        })?;
        Ok(Ntime(time))
    }
}

impl fmt::Display for Ntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0)
    }
}

impl From<Ntime> for u32 {
    fn from(n: Ntime) -> u32 {
        n.0
    }
}

impl From<u32> for Ntime {
    fn from(n: u32) -> Ntime {
        Ntime(n)
    }
}

impl TryFrom<u64> for Ntime {
    type Error = <u32 as TryFrom<u64>>::Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Ok(Ntime(u32::try_from(value)?))
    }
}
