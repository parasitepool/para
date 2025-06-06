use super::*;

// TODO: probably do target from difficulty
// caveat: if the target is very large it cannot be properly represented in the compact_lossy
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

fn target_as_block_hash(target: Target) -> BlockHash {
    BlockHash::from_raw_hash(Hash::from_byte_array(target.to_le_bytes()))
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

#[derive(Debug, Parser)]
pub(crate) struct Miner {}

impl Miner {
    pub(crate) fn run(&self) -> Result {
        let job_id = 123;
        let target = target(4);

        println!(
            "Mining...\nId\t\t{}\nTarget\t\t{}\nDifficulty\t{}\n\n",
            job_id,
            target,
            target.difficulty_float()
        );

        let mut hasher = Hasher {
            header: header(None, None),
            target,
        };

        let start = Instant::now();
        let header = hasher.hash()?;
        let duration = (Instant::now() - start).as_millis();

        if header.validate_pow(header.target()).is_ok() {
            println!("Block found!");
        } else {
            println!("Share found!");
        }

        println!(
            "Nonce\t\t{}\nTime\t\t{}ms\nBlockhash\t{}\nTarget\t\t{}\nWork\t\t{}\n",
            header.nonce,
            duration,
            header.block_hash(),
            target_as_block_hash(target),
            target.to_work(),
        );

        Ok(())
    }
}

// Comese from the mining.notify message
//struct Job {
//    job_id: u32,
//    prev_hash: [u8; 32],
//    coinbase_1: Vec<u32>,
//    coinbase_2: Vec<u32>,
//    merkle_brances: Vec<[u8; 32]>,
//    merkle_root: [u8; 32],
//    version: u32,
//    nbits: u32,
//    _ntime: u32,       // not needed?
//    _clean_jobs: bool, // not needed
//}
//
// Handles all the stratum protocol messages. Holds all the client information and updates the
// hasher with new work/templates. Has a couple channels to the Miner for communication and
// listens/talks to upstream mining pool
//struct Client {
//    client_id: u32,
//    extranonce1: Option<Extranonce<'static>>,
//    extranonce2_size: Option<usize>,
//    version_rolling_mask: Option<HexU32Be>,
//    version_rolling_min_bit: Option<HexU32Be>,
//    miner: Miner,
//}

// Implements the actual hashing and increments the nonce and checks if below pool target. For now
// should only increment the nonce space and not think too much about extranonce2 space. It has
// channels to the client for sending shares and updating workbase.
struct Hasher {
    header: Header,
    target: Target, // this is not necessarily the target from the pool but a custom one from client
}

impl Hasher {
    fn hash(&mut self) -> Result<Header> {
        loop {
            if self.target.is_met_by(self.header.block_hash()) {
                return Ok(self.header);
            }

            self.header.nonce += 1;

            if self.header.nonce == u32::MAX {
                return Err(anyhow!("nonce space exhausted"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
