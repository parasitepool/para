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
    base64::{Engine, engine::general_purpose},
    bip322::verify_simple_encoded,
    bitcoin::{
        Address, Amount, Block, BlockHash, CompactTarget, Network, OutPoint, ScriptBuf, Sequence,
        Target, Transaction, TxIn, TxMerkleNode, TxOut, Txid, VarInt, Witness,
        block::{self, Header},
        consensus::{self, Decodable, Encodable, encode},
        hashes::{Hash, sha256d},
        locktime::absolute::LockTime,
        script::write_scriptint,
        secp256k1::Secp256k1,
        sign_message::MessageSignature,
    },
    bitcoincore_rpc::{Auth, RpcApi},
    block_template::BlockTemplate,
    byteorder::{BigEndian, ByteOrder, LittleEndian},
    chain::Chain,
    clap::Parser,
    coinbase_builder::CoinbaseBuilder,
    connection::Connection,
    derive_more::Display,
    futures::{
        sink::SinkExt,
        stream::{FuturesUnordered, StreamExt},
    },
    generator::Generator,
    hex::FromHex,
    lazy_static::lazy_static,
    lru::LruCache,
    rand::RngCore,
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
        collections::{BTreeMap, HashMap, HashSet},
        env,
        fmt::{self, Display, Formatter},
        fs,
        io::{self, Write},
        net::{SocketAddr, ToSocketAddrs},
        num::NonZeroUsize,
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
    stratum::{
        Authorize, Configure, Difficulty, Extranonce, Id, JobId, JsonRpcError, MerkleNode, Message,
        Nbits, Nonce, Notify, Ntime, PrevHash, SetDifficulty, Submit, Subscribe, SubscribeResult,
        Version,
    },
    subcommand::{pool::pool_config::PoolConfig, server::account::Account},
    sysinfo::{Disks, System},
    tokio::{
        io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
        net::{TcpListener, TcpStream, tcp::OwnedWriteHalf},
        runtime::Runtime,
        signal::ctrl_c,
        sync::{Mutex, mpsc, oneshot, watch},
        task::{self, JoinHandle, JoinSet},
        time::{MissedTickBehavior, interval, sleep, timeout},
    },
    tokio_util::{
        codec::{FramedRead, FramedWrite, LinesCodec},
        sync::CancellationToken,
    },
    tower_http::{
        services::ServeDir, set_header::SetResponseHeaderLayer,
        validate_request::ValidateRequestHeaderLayer,
    },
    tracing::{debug, error, info, warn},
    tracing_subscriber::EnvFilter,
    zeromq::{Endpoint, Socket, SocketRecv, SubSocket},
    zmq::Zmq,
};

pub use subcommand::server::api;

mod arguments;
mod block_template;
mod chain;
pub mod ckpool;
pub mod coinbase_builder;
mod connection;
mod generator;
mod job;
mod jobs;
pub mod stratum;
pub mod subcommand;
mod zmq;

pub const COIN_VALUE: u64 = 100_000_000;
pub const USER_AGENT: &str = "para/0.5.2";

pub const EXTRANONCE1_SIZE: usize = 4;
pub const EXTRANONCE2_SIZE: usize = 8;
pub const MAX_MESSAGE_SIZE: usize = 32 * 1024;
/// Subscription IDs do not seem to have a purpose in Stratum, hardcoding for now
pub const SUBSCRIPTION_ID: &str = "deadbeef";
pub const LRU_CACHE_SIZE: usize = 256;

type Result<T = (), E = Error> = std::result::Result<T, E>;

fn target_as_block_hash(target: bitcoin::Target) -> BlockHash {
    BlockHash::from_raw_hash(Hash::from_byte_array(target.to_le_bytes()))
}

async fn resolve_stratum_endpoint(stratum_endpoint: &str) -> Result<SocketAddr> {
    let endpoint = if stratum_endpoint.contains(':') {
        stratum_endpoint.to_string()
    } else {
        format!("{}:42069", stratum_endpoint)
    };

    let addr = tokio::net::lookup_host(&endpoint)
        .await?
        .next()
        .with_context(|| "Failed to resolve hostname")?;

    Ok(addr)
}

pub fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
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
