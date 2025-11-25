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
        Target, Transaction, TxIn, TxOut, Txid, VarInt, Witness,
        block::{self, Header},
        consensus::{self, Decodable, encode},
        hashes::Hash,
        locktime::absolute::LockTime,
        script::write_scriptint,
        secp256k1::Secp256k1,
        sign_message::MessageSignature,
    },
    bitcoincore_rpc::{Auth, RpcApi},
    block_template::BlockTemplate,
    chain::Chain,
    clap::Parser,
    coinbase_builder::CoinbaseBuilder,
    connection::Connection,
    futures::{
        sink::SinkExt,
        stream::{FuturesUnordered, StreamExt},
    },
    generator::Generator,
    lru::LruCache,
    reqwest::Url,
    rust_embed::RustEmbed,
    rustls_acme::{
        AcmeConfig,
        acme::{LETS_ENCRYPT_PRODUCTION_DIRECTORY, LETS_ENCRYPT_STAGING_DIRECTORY},
        axum::AxumAcceptor,
        caches::DirCache,
    },
    serde::{
        Deserialize, Serialize,
        de::{self, Deserializer},
    },
    serde_json::json,
    serde_with::{DeserializeFromStr, SerializeDisplay},
    snafu::Snafu,
    sqlx::{Pool, Postgres, postgres::PgPoolOptions},
    std::{
        collections::{BTreeMap, HashMap, HashSet},
        env,
        fmt::{self, Display, Formatter},
        fs,
        io::{self, Write},
        net::{SocketAddr, ToSocketAddrs},
        num::NonZeroUsize,
        ops::Add,
        path::{Path, PathBuf},
        process,
        str::FromStr,
        sync::{
            Arc, LazyLock,
            atomic::{AtomicU64, Ordering},
        },
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    },
    stratum::{
        Authorize, Configure, Difficulty, Extranonce, Id, JobId, MerkleNode, Message, Nbits, Nonce,
        Notify, Ntime, PrevHash, SetDifficulty, StratumError, Submit, Subscribe, SubscribeResult,
        Version,
    },
    subcommand::{pool::pool_config::PoolConfig, server::account::Account},
    sysinfo::{Disks, System},
    tokio::{
        io::{AsyncRead, AsyncWrite},
        net::TcpListener,
        runtime::Runtime,
        sync::{Mutex, broadcast, mpsc, watch},
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
    tracing_appender::non_blocking,
    tracing_subscriber::EnvFilter,
    workbase::Workbase,
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
mod signal;
pub mod stratum;
pub mod subcommand;
mod workbase;
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

fn integration_test() -> bool {
    std::env::var_os("PARA_INTEGRATION_TEST").is_some()
}

fn logs_enabled() -> bool {
    std::env::var_os("RUST_LOG").is_some()
}

pub fn main() {
    let (writer, _guard) = non_blocking(io::stderr());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .with_writer(writer)
        .init();

    let args = Arguments::parse();

    Runtime::new()
        .expect("Failed to create tokio runtime")
        .block_on(async {
            let cancel_token = signal::setup_signal_handler();

            match args.run(cancel_token).await {
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
        });
}
