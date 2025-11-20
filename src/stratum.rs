use {
    bitcoin::{
        BlockHash, CompactTarget, Target, TxMerkleNode, Txid, block,
        consensus::Encodable,
        hashes::{Hash, sha256d},
    },
    byteorder::{BigEndian, ByteOrder, LittleEndian},
    derive_more::Display,
    error::{InternalError, Result},
    hex::FromHex,
    lazy_static::lazy_static,
    rand::RngCore,
    serde::{
        Deserialize, Serialize, Serializer,
        de::{self, Deserializer},
        ser::SerializeSeq,
    },
    serde_json::Value,
    serde_with::{DeserializeFromStr, SerializeDisplay},
    snafu::{ResultExt, Snafu},
    std::{
        collections::BTreeMap,
        fmt,
        ops::{BitAnd, BitOr, BitXor, Not},
        str::FromStr,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    },
    tokio::{
        io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader, BufWriter},
        net::{TcpStream, tcp::OwnedWriteHalf},
        sync::{Mutex, mpsc, oneshot},
        task::JoinHandle,
    },
    tracing::{debug, error, warn},
};

mod authorize;
mod client;
mod configure;
mod difficulty;
mod error;
mod extranonce;
mod job_id;
mod merkle;
mod message;
mod nbits;
mod nonce;
mod notify;
mod ntime;
mod prevhash;
mod set_difficulty;
mod submit;
mod subscribe;
mod suggest_difficulty;
mod version;

pub use {
    authorize::Authorize,
    client::Client,
    configure::Configure,
    difficulty::Difficulty,
    error::{JsonRpcError, StratumErrorCode},
    extranonce::Extranonce,
    job_id::JobId,
    merkle::{MerkleNode, merkle_branches, merkle_root},
    message::{Id, Message},
    nbits::Nbits,
    nonce::Nonce,
    notify::Notify,
    ntime::Ntime,
    prevhash::PrevHash,
    set_difficulty::SetDifficulty,
    submit::Submit,
    subscribe::{Subscribe, SubscribeResult},
    suggest_difficulty::SuggestDifficulty,
    version::Version,
};
