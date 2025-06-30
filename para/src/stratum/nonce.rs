use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, DeserializeFromStr, SerializeDisplay)]
pub struct Nonce(u32);

impl FromStr for Nonce {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let nonce = u32::from_str_radix(s, 16)?;
        Ok(Nonce(nonce))
    }
}

impl fmt::Display for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0)
    }
}

impl From<Nonce> for u32 {
    fn from(n: Nonce) -> u32 {
        n.0
    }
}

impl From<u32> for Nonce {
    fn from(n: u32) -> Nonce {
        Nonce(n)
    }
}
