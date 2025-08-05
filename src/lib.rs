#![allow(clippy::too_many_arguments)]
use {
    anyhow::{Error, anyhow, ensure},
    arguments::Arguments,
    axum::{
        Extension, Router,
        extract::{Json, Path},
        http::{
            self, HeaderValue, StatusCode,
            header::{CONTENT_DISPOSITION, CONTENT_TYPE},
        },
        response::{IntoResponse, Response},
        routing::{MethodRouter, get},
    },
    axum_server::Handle,
    bitcoin::{
        BlockHash, CompactTarget, Target, TxMerkleNode,
        block::{self, Header},
        consensus::Decodable,
        hashes::{Hash, sha256d},
    },
    byteorder::{BigEndian, ByteOrder, LittleEndian},
    clap::Parser,
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
    serde_json::Value,
    serde_with::{DeserializeFromStr, SerializeDisplay},
    sqlx::{Pool, Postgres, postgres::PgPoolOptions},
    std::{
        collections::{BTreeMap, HashMap},
        env, fmt, fs, io,
        net::ToSocketAddrs,
        ops::Add,
        path::PathBuf,
        process,
        str::FromStr,
        sync::{
            Arc, LazyLock,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    },
    stratum::{Id, Message, Notify, SetDifficulty},
    sysinfo::{Disks, System},
    tokio::{
        io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader, BufWriter},
        net::{TcpStream, tcp::OwnedWriteHalf},
        runtime::Runtime,
        signal::ctrl_c,
        sync::{Mutex, mpsc, oneshot},
        task::{self, JoinHandle},
    },
    tokio_util::sync::CancellationToken,
    tower_http::{
        services::ServeDir, set_header::SetResponseHeaderLayer,
        validate_request::ValidateRequestHeaderLayer,
    },
    tracing::{debug, error, info, warn},
    tracing_subscriber::EnvFilter,
};

mod arguments;
pub mod difficulty;
pub mod hash_rate;
pub mod stratum;
mod subcommand;

pub const COIN_VALUE: u64 = 100_000_000;

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
