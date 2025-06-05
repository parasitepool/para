use {bitcoin::block::Header, primitive_types::U256};

// This could also be called the Hasher, it implements the actual hashing and increments the nonce
// and checks if below pool target. For now should only increment the nonce space and not think too
// much about extranonce2 space. It has channels to the client for sending shares and updating
// workbase.
struct Miner {
    header: Header,
    target: U256, // this is not necessarily the target from the pool but a custom one from client
    job_id: u32,
}

// Comese from the mining.notify message
struct Job {
    job_id: u32,
    prev_hash: [u8; 32],
    coinbase_1: Vec<u32>,
    coinbase_2: Vec<u32>,
    merkle_brances: Vec<[u8; 32]>,
    merkle_root: [u8; 32],
    version: u32,
    nbits: u32,
    _ntime: u32,       // not needed?
    _clean_jobs: bool, // not needed
}

// Handles all the stratum protocol messages. Holds all the client information and updates the
// miner with new work/templates. Has a couple channels to the Miner for communication and
// listens/talks to upstream mining pool
struct Client {
    client_id: u32,
    extranonce1: Option<Extranonce<'static>>,
    extranonce2_size: Option<usize>,
    version_rolling_mask: Option<HexU32Be>,
    version_rolling_min_bit: Option<HexU32Be>,
    miner: Miner,
}
