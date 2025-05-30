#![allow(clippy::too_many_arguments)]
use {
    anyhow::{Error, anyhow, ensure},
    arguments::Arguments,
    axum::{
        Extension, Router,
        extract::{Json, Path, Request},
        http::{
            HeaderMap, HeaderValue, StatusCode,
            header::{AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_TYPE},
        },
        middleware::{self, Next},
        response::{IntoResponse, Response},
        routing::get,
    },
    axum_server::Handle,
    base64::{Engine, engine::general_purpose::STANDARD},
    clap::Parser,
    database::Database,
    futures::stream::StreamExt,
    options::Options,
    rust_embed::RustEmbed,
    rustls_acme::{
        AcmeConfig,
        acme::{LETS_ENCRYPT_PRODUCTION_DIRECTORY, LETS_ENCRYPT_STAGING_DIRECTORY},
        axum::AxumAcceptor,
        caches::DirCache,
    },
    serde::{Deserialize, Serialize},
    sqlx::{Pool, Postgres, postgres::PgPoolOptions},
    std::{
        env,
        fmt::Display,
        io,
        net::ToSocketAddrs,
        path::PathBuf,
        process,
        sync::{Arc, LazyLock},
    },
    sysinfo::{Disks, System},
    tokio::{runtime::Runtime, task},
    tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer},
};

mod arguments;
mod database;
mod options;
mod subcommand;
mod templates;
mod util;

pub const COIN_VALUE: u64 = 100_000_000;

type Result<T = (), E = Error> = std::result::Result<T, E>;

pub fn main() {
    env_logger::init();

    let args = Arguments::parse();

    match args.run() {
        Err(err) => {
            eprintln!("error: {err}");

            if env::var_os("RUST_BACKTRACE")
                .map(|val| val == "1")
                .unwrap_or_default()
            {
                eprintln!("{}", err.backtrace());
            }
            process::exit(1);
        }
        Ok(_) => {
            process::exit(0);
        }
    }
}
