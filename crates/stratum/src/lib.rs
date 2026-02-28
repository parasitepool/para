use {
    bitcoin::{
        Address, BlockHash, CompactTarget, Network, Target, TxMerkleNode, Txid,
        address::NetworkUnchecked,
        block,
        consensus::Encodable,
        hashes::{Hash, sha256d},
    },
    byteorder::{BigEndian, ByteOrder, LittleEndian},
    derive_more::Display,
    hex::FromHex,
    rand::RngCore,
    serde::{
        Deserialize, Serialize, Serializer,
        de::{self, Deserializer},
        ser::SerializeSeq,
    },
    serde_json::Value,
    serde_with::{DeserializeFromStr, SerializeDisplay},
    smallvec::{SmallVec, smallvec},
    snafu::{ResultExt, Snafu},
    std::{
        fmt::{self, Display, Formatter},
        ops::{BitAnd, BitOr, BitXor, Not},
        str::FromStr,
        sync::LazyLock,
    },
};

pub use {
    difficulty::Difficulty,
    error::{InternalError, Result, StratumError, StratumErrorResponse},
    extranonce::Extranonce,
    job_id::JobId,
    merkle::{MerkleNode, merkle_branches, merkle_root},
    message::{Id, Message},
    method::{
        Authorize, Configure, ConfigureResponse, Notify, Reconnect, SetDifficulty, Submit,
        Subscribe, SubscribeResponse, SuggestDifficulty,
    },
    nbits::Nbits,
    nonce::Nonce,
    ntime::Ntime,
    prevhash::PrevHash,
    si::{format_si, parse_si},
    username::Username,
    version::Version,
};

pub const MAX_MESSAGE_SIZE: usize = 32 * 1024;

pub mod error;
pub mod message;
pub mod method;

#[cfg(feature = "client")]
pub mod client;

mod difficulty;
mod extranonce;
mod job_id;
mod merkle;
mod nbits;
mod nonce;
mod ntime;
mod prevhash;
mod si;
mod username;
mod version;
