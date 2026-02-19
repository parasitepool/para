use {
    anyhow::{Context, Error, anyhow, bail, ensure},
    arguments::Arguments,
    async_trait::async_trait,
    axum::{
        Extension, Json, Router,
        extract::{DefaultBodyLimit, FromRequestParts},
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
        address::NetworkUnchecked,
        block::{self, Header},
        consensus::{self, Decodable, encode},
        hashes::Hash,
        locktime::absolute::LockTime,
        script::write_scriptint,
        secp256k1::Secp256k1,
        sign_message::MessageSignature,
    },
    bitcoind_async_client::{
        traits::Reader,
        {Auth, Client},
    },
    block_template::BlockTemplate,
    boilerplate::{Boilerplate, Trusted},
    chain::Chain,
    clap::{Args, Parser},
    coinbase_builder::CoinbaseBuilder,
    dashmap::DashMap,
    decay::{DecayingAverage, calculate_time_bias},
    extranonces::{Extranonces, PoolExtranonces, ProxyExtranonces},
    futures::{
        sink::SinkExt,
        stream::{FuturesUnordered, StreamExt},
    },
    generator::spawn_generator,
    hashrate::HashRate,
    job::Job,
    jobs::Jobs,
    logs::logs_enabled,
    lru::LruCache,
    metatron::Metatron,
    metrics::Metrics,
    parking_lot::Mutex,
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
    settings::{PoolOptions, ProxyOptions, Settings},
    snafu::Snafu,
    sqlx::{Pool, Postgres, postgres::PgPoolOptions},
    std::{
        collections::{BTreeMap, HashMap, HashSet, VecDeque},
        env,
        fmt::{self, Display, Formatter},
        fs,
        io::{self, Write},
        net::{SocketAddr, ToSocketAddrs},
        num::NonZeroUsize,
        ops::{Add, AddAssign, Div, Mul, Sub, SubAssign},
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
    stratifier::{SessionSnapshot, Stratifier},
    stratum::{
        Authorize, Configure, Difficulty, Extranonce, Id, JobId, MerkleNode, Message, Nbits, Nonce,
        Notify, Ntime, PrevHash, SetDifficulty, StratumError, Submit, Subscribe, SubscribeResult,
        Username, Version, format_si, parse_si,
    },
    subcommand::server::account::Account,
    sysinfo::{Disks, System},
    throbber::{StatusLine, spawn_throbber},
    tokio::{
        net::{
            TcpListener, TcpStream,
            tcp::{OwnedReadHalf, OwnedWriteHalf},
        },
        runtime::Runtime,
        sync::{RwLock, broadcast, mpsc, watch},
        task::{self, JoinHandle, JoinSet},
        time::{MissedTickBehavior, interval, sleep, timeout},
    },
    tokio_util::{
        codec::{FramedRead, FramedWrite, LinesCodec},
        sync::CancellationToken,
    },
    tracing::{Subscriber, debug, error, info, warn},
    tracing_appender::non_blocking,
    tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt},
    upstream::Upstream,
    user::User,
    utoipa::{OpenApi, ToSchema},
    vardiff::Vardiff,
    workbase::Workbase,
    worker::Worker,
    zeromq::{Endpoint, Socket, SocketRecv, SubSocket},
    zmq::Zmq,
};

pub mod api;
mod arguments;
mod block_template;
mod chain;
pub mod ckpool;
mod coinbase_builder;
mod decay;
mod event_sink;
mod extranonces;
mod generator;
pub mod hashrate;
mod http_server;
mod job;
mod jobs;
mod logs;
mod metatron;
mod metrics;
mod session;
pub mod settings;
mod signal;
mod stratifier;
pub mod stratum;
pub mod subcommand;
mod throbber;
mod upstream;
mod user;
mod vardiff;
mod workbase;
mod worker;
mod zmq;

pub const COIN_VALUE: u64 = 100_000_000;
pub const USER_AGENT: &str = "para/0.5.2";
pub const MIN_ENONCE_SIZE: usize = 2;
pub const MAX_ENONCE_SIZE: usize = 8;
pub const ENONCE1_SIZE: usize = 4;
pub const ENONCE1_EXTENSION_SIZE: usize = 2;
pub const MAX_MESSAGE_SIZE: usize = 32 * 1024;
pub const SHARE_CHANNEL_CAPACITY: usize = 100_000;
pub const SUBSCRIPTION_ID: &str = "deadbeef";
pub const LRU_CACHE_SIZE: usize = 256;
pub const SESSION_TTL: Duration = Duration::from_secs(600);
/// Max ntime forward roll in seconds. Conservative margin under Bitcoin's 2-hour limit.
pub const MAX_NTIME_OFFSET: u32 = 7000;

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

pub fn main() {
    let (logs, _guard) = logs::init();

    let args = Arguments::parse();

    Runtime::new()
        .expect("Failed to create tokio runtime")
        .block_on(async {
            let cancel_token = signal::setup_signal_handler();

            match args.run(cancel_token, logs).await {
                Err(err) => {
                    eprintln!("error: {err}");

                    for (i, cause) in err.chain().skip(1).enumerate() {
                        if i == 0 {
                            eprintln!();
                            eprintln!("because:");
                        }
                        eprintln!("- {cause}");
                    }

                    if env::var_os("RUST_BACKTRACE")
                        .map(|val| val == "1")
                        .unwrap_or_default()
                    {
                        eprintln!();
                        eprintln!("{}", err.backtrace());
                    }
                    process::exit(1);
                }
                Ok(_) => {
                    process::exit(0);
                }
            }
        });
}
