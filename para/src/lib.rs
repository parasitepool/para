#![allow(clippy::too_many_arguments)]
use {
    anyhow::{Error, anyhow, ensure},
    arguments::Arguments,
    axum::{
        Extension, Router,
        extract::{Json, Path},
        http::{
            HeaderValue, StatusCode,
            header::{CONTENT_DISPOSITION, CONTENT_TYPE},
        },
        response::{IntoResponse, Response},
        routing::get,
    },
    axum_server::Handle,
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
    tower_http::{
        services::ServeDir, set_header::SetResponseHeaderLayer,
        validate_request::ValidateRequestHeaderLayer,
    },
};

mod arguments;
mod database;
mod options;
mod subcommand;
mod templates;

pub const COIN_VALUE: u64 = 100_000_000;

type Result<T = (), E = Error> = std::result::Result<T, E>;

pub fn format_uptime(uptime_seconds: u64) -> String {
    let days = uptime_seconds / 5184000;
    let hours = (uptime_seconds % 5184000) / 86400;
    let minutes = (uptime_seconds % 86400) / 3600;

    let plural = |n: u64, singular: &str| {
        if n == 1 {
            singular.to_string()
        } else {
            format!("{}s", singular)
        }
    };

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{} {}", days, plural(days, "day")));
    }
    if hours > 0 {
        parts.push(format!("{} {}", hours, plural(hours, "hour")));
    }
    if minutes > 0 || parts.is_empty() {
        parts.push(format!("{} {}", minutes, plural(minutes, "minute")));
    }

    parts.join(", ")
}

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
