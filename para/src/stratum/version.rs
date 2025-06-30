use super::*;

#[derive(Debug, Clone, PartialEq, Eq, DeserializeFromStr, SerializeDisplay)]
pub struct Version(pub block::Version);

impl FromStr for Version {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let n = u32::from_str_radix(s, 16)?;
        // The as conversion matches Bitcoin's behaviour
        Ok(Self(block::Version::from_consensus(n as i32)))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0.to_consensus())
    }
}

impl From<block::Version> for Version {
    fn from(v: block::Version) -> Self {
        Self(v)
    }
}

impl From<Version> for block::Version {
    fn from(v: Version) -> Self {
        v.0
    }
}

impl AsRef<block::Version> for Version {
    fn as_ref(&self) -> &block::Version {
        &self.0
    }
}

impl std::ops::Deref for Version {
    type Target = block::Version;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn case(version_str: &str, expected_consensus: i32) {
        let version = Version::from_str(version_str).unwrap();

        assert_eq!(
            version.to_string(),
            version_str,
            "Display round-trip failed"
        );

        assert_eq!(
            version.0.to_consensus(),
            expected_consensus,
            "Consensus i32 value mismatch"
        );

        let as_block_version: block::Version = version.clone().into();
        assert_eq!(
            as_block_version.to_consensus(),
            expected_consensus,
            "Into<block::Version> failed"
        );

        let from_block = Version::from(as_block_version);
        assert_eq!(
            from_block, version,
            "From<block::Version> round-trip failed"
        );

        let serialized = serde_json::to_string(&version).unwrap();
        assert_eq!(
            serialized,
            format!("\"{}\"", version_str),
            "Serialization mismatch"
        );

        let deserialized = serde_json::from_str::<Version>(&serialized).unwrap();
        assert_eq!(deserialized, version, "Deserialization round-trip failed");
    }

    #[test]
    fn version_bip9_signaling_default() {
        case("20000000", 0x20000000);
    }

    #[test]
    fn version_negative() {
        case("ffffffff", -1);
    }

    #[test]
    fn version_feature_bits_set() {
        case("00000001", 1);
    }

    #[test]
    fn version_bip9_with_feature_bits() {
        case("20000002", 0x20000002);
    }
}
