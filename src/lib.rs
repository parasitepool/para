#![allow(clippy::too_many_arguments)]
use {
    anyhow::{Context, Error, anyhow, bail, ensure},
    arguments::Arguments,
    axum::{
        Extension, Router,
        extract::{DefaultBodyLimit, Json},
        http::{
            self, HeaderValue, StatusCode,
            header::{CONTENT_DISPOSITION, CONTENT_TYPE},
        },
        response::{IntoResponse, Response},
        routing::{MethodRouter, get, post},
    },
    axum_server::Handle,
    bitcoin::{
        Address, Amount, BlockHash, CompactTarget, Network, OutPoint, ScriptBuf, Sequence, Target,
        Transaction, TxIn, TxMerkleNode, TxOut, Txid, Witness,
        block::{self, Header},
        consensus::{self, Decodable, Encodable},
        hashes::{Hash, sha256d},
        locktime::absolute::LockTime,
        script::write_scriptint,
    },
    bitcoincore_rpc::{Auth, RpcApi, json::GetBlockTemplateResult},
    byteorder::{BigEndian, ByteOrder, LittleEndian},
    chain::Chain,
    clap::Parser,
    coinbase_builder::CoinbaseBuilder,
    derive_more::Display,
    difficulty::Difficulty,
    futures::stream::StreamExt,
    hash_rate::HashRate,
    hex::FromHex,
    lazy_static::lazy_static,
    rand::Rng,
    reqwest::Url,
    rust_embed::RustEmbed,
    rustls_acme::{
        AcmeConfig,
        acme::{LETS_ENCRYPT_PRODUCTION_DIRECTORY, LETS_ENCRYPT_STAGING_DIRECTORY},
        axum::AxumAcceptor,
        caches::DirCache,
    },
    serde::{
        Deserialize, Serialize, Serializer,
        de::{self, Deserializer},
        ser::SerializeSeq,
    },
    serde_json::{Value, json},
    serde_with::{DeserializeFromStr, SerializeDisplay},
    sqlx::{Pool, Postgres, postgres::PgPoolOptions},
    std::{
        collections::{BTreeMap, HashMap},
        env,
        fmt::{self, Display, Formatter},
        fs, io,
        net::{SocketAddr, ToSocketAddrs},
        ops::{Add, BitAnd, BitOr, BitXor, Not},
        path::{Path, PathBuf},
        process,
        str::FromStr,
        sync::{
            Arc, LazyLock,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    },
    stratifier::Connection,
    stratum::{
        Authorize, Configure, Id, Message, Nbits, Notify, Ntime, PrevHash, SetDifficulty, Submit,
        Subscribe, SubscribeResult, Version,
    },
    sysinfo::{Disks, System},
    tokio::{
        io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
        net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
        runtime::Runtime,
        signal::ctrl_c,
        sync::{Mutex, mpsc, oneshot},
        task::{self, JoinHandle},
        time::sleep,
    },
    tokio_util::sync::CancellationToken,
    tower_http::{
        services::ServeDir, set_header::SetResponseHeaderLayer,
        validate_request::ValidateRequestHeaderLayer,
    },
    tracing::{debug, error, info, warn},
    tracing_subscriber::EnvFilter,
};

pub use subcommand::server::api;

mod arguments;
mod chain;
pub mod ckpool;
pub mod coinbase_builder;
pub mod difficulty;
pub mod hash_rate;
pub mod stratifier;
pub mod stratum;
pub mod subcommand;

pub const COIN_VALUE: u64 = 100_000_000;
pub const USER_AGENT: &str = "paraminer/0.0.1";
// pub const EXTRANONCE1_SIZE: u32 = 4;
pub const EXTRANONCE2_SIZE: usize = 8;

type Result<T = (), E = Error> = std::result::Result<T, E>;

fn target_as_block_hash(target: bitcoin::Target) -> BlockHash {
    BlockHash::from_raw_hash(Hash::from_byte_array(target.to_le_bytes()))
}

pub fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Arguments::parse();

    match args.run() {
        Err(err) => {
            error!("error: {err}");

            if env::var_os("RUST_BACKTRACE")
                .map(|val| val == "1")
                .unwrap_or_default()
            {
                error!("{}", err.backtrace());
            }
            process::exit(1);
        }
        Ok(_) => {
            process::exit(0);
        }
    }
}
