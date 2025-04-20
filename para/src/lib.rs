#![allow(clippy::too_many_arguments)]
use {
    anyhow::{Error, anyhow, bail, ensure},
    arguments::Arguments,
    axum::{
        Extension, Router,
        http::{
            HeaderValue, StatusCode,
            header::{CONTENT_DISPOSITION, CONTENT_TYPE},
        },
        response::{IntoResponse, Response},
        routing::get,
    },
    axum_server::Handle,
    bitcoin::Network,
    bitcoincore_rpc::{Auth, Client, RpcApi},
    chain::Chain,
    clap::Parser,
    database::Database,
    futures::stream::StreamExt,
    options::Options,
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
        fmt::{self, Display, Formatter},
        io,
        net::ToSocketAddrs,
        path::{Path, PathBuf},
        process,
        str::FromStr,
        sync::{Arc, LazyLock},
        thread,
        time::Duration,
    },
    tokio::{runtime::Runtime, task},
    tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer},
};

mod arguments;
mod chain;
mod database;
mod options;
mod subcommand;

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
