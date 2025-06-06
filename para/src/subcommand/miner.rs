use super::*;

fn target() -> Target {
    let mut bytes = [0u8; 32];
    bytes[0] = 0x00;
    bytes[1] = 0x00;
    bytes[2] = 0xff;
    bytes[3] = 0xff;
    Target::from_be_bytes(bytes)
}

fn header(target: Target) -> Header {
    let hash = BlockHash::all_zeros().to_raw_hash();

    Header {
        version: Version::TWO,
        prev_blockhash: BlockHash::all_zeros(),
        merkle_root: TxMerkleNode::from_raw_hash(hash),
        time: 0,
        bits: target.to_compact_lossy(),
        nonce: 0,
    }
}

#[derive(Debug, Parser)]
pub(crate) struct Miner {}

impl Miner {
    pub(crate) fn run(&self) -> Result {
        let job_id = 123;
        let target = target();

        println!(
            "Mining\n\tId: {}\n\tTarget: {}\n\tDifficulty: {}\n\t",
            job_id,
            target,
            target.difficulty_float()
        );

        let mut hasher = Hasher {
            header: header(target),
            job_id,
            target,
        };

        let header = hasher.hash()?;

        println!("Found block with with nonce: {}", header.nonce);

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
// miner with new work/templates. Has a couple channels to the Miner for communication and
// listens/talks to upstream mining pool
//struct Client {
//    client_id: u32,
//    extranonce1: Option<Extranonce<'static>>,
//    extranonce2_size: Option<usize>,
//    version_rolling_mask: Option<HexU32Be>,
//    version_rolling_min_bit: Option<HexU32Be>,
//    miner: Miner,
//}

// This could also be called the Miner, it implements the actual hashing and increments the nonce
// and checks if below pool target. For now should only increment the nonce space and not think too
// much about extranonce2 space. It has channels to the client for sending shares and updating
// workbase.
struct Hasher {
    header: Header,
    job_id: u32,
    target: Target, // this is not necessarily the target from the pool but a custom one from client
}

impl Hasher {
    fn hash(&mut self) -> Result<Header> {
        println!("Hashing for job {}", self.job_id);
        loop {
            if self.target.is_met_by(self.header.block_hash()) {
                return Ok(self.header);
            }

            self.header.nonce += 1;

            if self.header.nonce == 100_000 {
                return Err(anyhow!("Hashed {} times", self.header.nonce));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hasher_hashes() {
        let target = target();

        let mut hasher = Hasher {
            header: header(target),
            job_id: 1,
            target,
        };

        let header = hasher.hash().unwrap();

        assert_eq!(header.nonce, 0);
        assert!(header.validate_pow(target).is_ok());
    }
}
