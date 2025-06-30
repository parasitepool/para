use super::*;

/// prevhash in the protocol spec is insane: it swaps bytes of every u32 word into big endian.
#[derive(Debug, PartialEq, Clone, DeserializeFromStr, SerializeDisplay)]
pub struct PrevHash(BlockHash);

impl FromStr for PrevHash {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = <[u8; 32]>::from_hex(s)?;

        let mut reordered = [0u8; 32];
        for (src, dst) in bytes.chunks_exact(4).zip(reordered.chunks_mut(4)) {
            let word = BigEndian::read_u32(src);
            LittleEndian::write_u32(dst, word);
        }

        let inner = BlockHash::from_slice(&reordered)?;
        Ok(PrevHash(inner))
    }
}

impl fmt::Display for PrevHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut swapped = [0u8; 32];
        for (src, dst) in self
            .0
            .as_byte_array()
            .chunks_exact(4)
            .zip(swapped.chunks_mut(4))
        {
            let word = LittleEndian::read_u32(src);
            BigEndian::write_u32(dst, word);
        }

        write!(f, "{}", hex::encode(swapped))
    }
}

impl From<BlockHash> for PrevHash {
    fn from(blockhash: BlockHash) -> Self {
        PrevHash(blockhash)
    }
}

impl From<PrevHash> for BlockHash {
    fn from(prevhash: PrevHash) -> Self {
        prevhash.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn case(prevhash_str: &str, blockhash_str: &str) {
        let prevhash = prevhash_str.parse::<PrevHash>().unwrap();
        assert_eq!(prevhash.to_string(), prevhash_str);

        let blockhash = BlockHash::from_str(blockhash_str).unwrap();
        assert_eq!(prevhash.0, blockhash);
        assert_eq!(BlockHash::from(prevhash.clone()), blockhash);
        assert_eq!(prevhash, PrevHash::from(blockhash));

        let serialized = serde_json::to_string(&prevhash).unwrap();
        assert_eq!(serialized, format!("\"{}\"", prevhash_str));

        let round_trip = serde_json::from_str::<PrevHash>(&serialized).unwrap();
        assert_eq!(round_trip, prevhash);
    }

    #[test]
    fn roundtrip_prevhash() {
        case(
            "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000",
            "00000000440b921e1b77c6c0487ae5616de67f788f44ae2a5af6e2194d16b6f8",
        );
    }

    #[test]
    fn roundtrip_another_prevhash() {
        case(
            "899cec175f2a0d2d6c05769137d3c09a536ae9a368bdbc7309efa16c0000030e",
            "0000030e09efa16c68bdbc73536ae9a337d3c09a6c0576915f2a0d2d899cec17",
        );
    }
}
