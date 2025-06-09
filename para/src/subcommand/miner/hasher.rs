use super::*;

// Implements the actual hashing and increments the nonce and checks if below pool target. For now
// should only increment the nonce space and not think too much about extranonce2 space. It has
// channels to the client for sending shares and updating workbase. Target is the one given from
// the pool. The actual network target/difficulty is inside the Header.
pub(crate) struct Hasher {
    pub(crate) header: Header,
    pub(crate) target: Target,
}

impl Hasher {
    #[allow(unused)]
    pub(crate) fn hash(&mut self) -> Result<Header> {
        loop {
            if self.target.is_met_by(self.header.block_hash()) {
                return Ok(self.header);
            }

            if self.header.nonce == u32::MAX {
                return Err(anyhow!("nonce space exhausted"));
            }

            self.header.nonce = self.header.nonce.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        bitcoin::{
            BlockHash, Target, TxMerkleNode,
            block::{Header, Version},
            hashes::Hash,
        },
    };

    // Hacky way to create very easy to solve targets for testing
    fn target(shift: u8) -> Target {
        let mut bytes = [0u8; 32];
        let (a, b, c, d) = match shift {
            0 => (0xff, 0xff, 0x00, 0x00),
            1 => (0x0f, 0xff, 0xf0, 0x00),
            2 => (0x00, 0xff, 0xff, 0x00),
            3 => (0x00, 0x0f, 0xff, 0xf0),
            4 => (0x00, 0x00, 0xff, 0xff),
            _ => panic!("shift should be less than 5"),
        };

        bytes[0] = a;
        bytes[1] = b;
        bytes[2] = c;
        bytes[3] = d;

        Target::from_be_bytes(bytes)
    }

    fn header(network_target: Option<Target>, nonce: Option<u32>) -> Header {
        Header {
            version: Version::TWO,
            prev_blockhash: BlockHash::all_zeros(),
            merkle_root: TxMerkleNode::from_raw_hash(BlockHash::all_zeros().to_raw_hash()),
            time: 0,
            bits: network_target.unwrap_or(Target::MAX).to_compact_lossy(),
            nonce: nonce.unwrap_or_default(),
        }
    }
    #[test]
    fn hasher_hashes() {
        let target = target(1);

        let mut hasher = Hasher {
            header: header(Some(target), None),
            target,
        };

        let header = hasher.hash().unwrap();

        assert!(header.validate_pow(target).is_ok());
    }

    #[test]
    fn hasher_nonce_space_exhausted() {
        let target = target(1);

        let mut hasher = Hasher {
            header: header(Some(target), Some(u32::MAX - 1)),
            target,
        };

        assert!(
            hasher
                .hash()
                .is_err_and(|err| err.to_string() == "nonce space exhausted")
        )
    }
}
