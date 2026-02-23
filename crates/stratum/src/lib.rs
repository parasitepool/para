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
    snafu::{ResultExt, Snafu},
    std::{
        fmt::{self, Display, Formatter},
        ops::{BitAnd, BitOr, BitXor, Not},
        str::FromStr,
        sync::LazyLock,
    },
};

pub use {
    authorize::Authorize,
    configure::{Configure, ConfigureResponse},
    difficulty::Difficulty,
    error::{InternalError, Result, StratumError, StratumErrorResponse},
    event::Event,
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
    si::{format_si, parse_si},
    submit::Submit,
    subscribe::{Subscribe, SubscribeResult},
    suggest_difficulty::SuggestDifficulty,
    username::Username,
    version::Version,
};

#[cfg(feature = "client")]
pub use client::{Client, ClientError, EventReceiver};

#[cfg(feature = "client")]
pub const MAX_MESSAGE_SIZE: usize = 32 * 1024;

mod authorize;
mod configure;
mod difficulty;
mod error;
mod event;
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
mod si;
mod submit;
mod subscribe;
mod suggest_difficulty;
mod username;
mod version;

#[cfg(feature = "client")]
mod client;
