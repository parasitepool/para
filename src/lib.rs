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
    clap::Parser,
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
    lru::LruCache,
    metatron::Metatron,
    metrics::Metrics,
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
            Arc, LazyLock, RwLock,
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
        sync::{Mutex, broadcast, mpsc, watch},
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
mod logstream;
mod metatron;
mod metrics;
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

fn logs_enabled() -> bool {
    std::env::var_os("RUST_LOG").is_some()
}

fn logstream_filter() -> EnvFilter {
    let level = CURRENT_LOG_LEVEL
        .read()
        .ok()
        .filter(|l| !l.is_empty())
        .map(|l| l.clone());

    EnvFilter::new(level.as_deref().unwrap_or("info"))
}

static CURRENT_LOG_LEVEL: LazyLock<Arc<RwLock<String>>> =
    LazyLock::new(|| Arc::new(RwLock::new(String::from("info"))));

type ReloadFn = Box<dyn Fn(EnvFilter) -> Result<()> + Send + Sync>;

type ReloadHandles = (
    ReloadFn,
    ReloadFn,
    tracing_appender::non_blocking::WorkerGuard,
);

static FILTER_HANDLE: LazyLock<ReloadHandles> = LazyLock::new(|| {
    let (writer, guard) = non_blocking(io::stderr());

    let fmt_filter = EnvFilter::from_default_env();
    let (fmt_filter, fmt_reload_handle) = tracing_subscriber::reload::Layer::new(fmt_filter);

    let ls_filter = logstream_filter();
    let (ls_filter, ls_reload_handle) = tracing_subscriber::reload::Layer::new(ls_filter);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_writer(writer)
                .with_filter(fmt_filter),
        )
        .with(logstream::LogStreamLayer.with_filter(ls_filter))
        .init();

    let fmt_reload = Box::new(move |f: EnvFilter| {
        fmt_reload_handle
            .reload(f)
            .context("failed to reload fmt filter")
    });

    let ls_reload = Box::new(move |f: EnvFilter| {
        ls_reload_handle
            .reload(f)
            .context("failed to reload logstream filter")
    });

    (fmt_reload, ls_reload, guard)
});

pub fn reload_log_filter() -> Result<()> {
    if let Ok(mut level) = CURRENT_LOG_LEVEL.write() {
        *level = String::new();
    }

    (FILTER_HANDLE.0)(EnvFilter::from_default_env())?;
    (FILTER_HANDLE.1)(logstream_filter())?;
    info!("Log filter reloaded from environment");
    Ok(())
}

pub fn set_log_level_runtime(level: &str) -> Result<()> {
    let new_filter = EnvFilter::new(level);

    (FILTER_HANDLE.1)(new_filter)?;

    if let Ok(mut current) = CURRENT_LOG_LEVEL.write() {
        *current = level.to_string();
    }

    info!("Log level changed to: {}", level);
    Ok(())
}

pub fn get_current_log_level() -> String {
    CURRENT_LOG_LEVEL
        .read()
        .map(|l| l.clone())
        .unwrap_or_else(|_| String::from("info"))
}

pub fn main() {
    let _ = &*FILTER_HANDLE;

    let args = Arguments::parse();

    Runtime::new()
        .expect("Failed to create tokio runtime")
        .block_on(async {
            let cancel_token = signal::setup_signal_handler();

            match args.run(cancel_token).await {
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
