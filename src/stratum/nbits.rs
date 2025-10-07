use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, DeserializeFromStr, SerializeDisplay)]
pub struct Nbits(CompactTarget);

impl Nbits {
    pub fn to_compact(&self) -> CompactTarget {
        self.0
    }
}

impl FromStr for Nbits {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let compact = CompactTarget::from_unprefixed_hex(s)?;
        Ok(Nbits(compact))
    }
}

impl fmt::Display for Nbits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0.to_consensus())
    }
}

impl From<Nbits> for CompactTarget {
    fn from(n: Nbits) -> CompactTarget {
        n.0
    }
}

impl From<CompactTarget> for Nbits {
    fn from(n: CompactTarget) -> Nbits {
        Nbits(n)
    }
}
