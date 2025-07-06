use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, DeserializeFromStr, SerializeDisplay)]
pub struct Ntime(u32);

impl FromStr for Ntime {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let time = u32::from_str_radix(s, 16)?;
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
