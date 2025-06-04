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
    let days = uptime_seconds / 86400;
    let hours = (uptime_seconds % 86400) / 3600;
    let minutes = (uptime_seconds % 3600) / 60;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_seconds() {
        assert_eq!(format_uptime(0), "0 minutes");
    }

    #[test]
    fn test_single_units() {
        assert_eq!(format_uptime(1), "0 minutes"); // Less than a minute
        assert_eq!(format_uptime(60), "1 minute");
        assert_eq!(format_uptime(3600), "1 hour");
        assert_eq!(format_uptime(86400), "1 day");
    }

    #[test]
    fn test_plural_units() {
        assert_eq!(format_uptime(120), "2 minutes");
        assert_eq!(format_uptime(7200), "2 hours");
        assert_eq!(format_uptime(172800), "2 days");
    }

    #[test]
    fn test_mixed_units() {
        assert_eq!(format_uptime(90060), "1 day, 1 hour, 1 minute");
        assert_eq!(format_uptime(183900), "2 days, 3 hours, 5 minutes");
        assert_eq!(format_uptime(88200), "1 day, 30 minutes");
        assert_eq!(format_uptime(8100), "2 hours, 15 minutes");
    }

    #[test]
    fn test_edge_cases() {
        assert_eq!(format_uptime(59), "0 minutes");
        assert_eq!(format_uptime(3599), "59 minutes");
        assert_eq!(format_uptime(86399), "23 hours, 59 minutes");

        assert_eq!(format_uptime(60), "1 minute");
        assert_eq!(format_uptime(3600), "1 hour");
        assert_eq!(format_uptime(86400), "1 day");
    }

    #[test]
    fn test_large_values() {
        assert_eq!(format_uptime(2592000), "30 days");

        assert_eq!(format_uptime(31581000), "365 days, 12 hours, 30 minutes");
    }

    #[test]
    fn test_only_minutes_when_less_than_hour() {
        assert_eq!(format_uptime(30), "0 minutes");
        assert_eq!(format_uptime(90), "1 minute");
        assert_eq!(format_uptime(1800), "30 minutes");
    }

    #[test]
    fn test_fractional_seconds_truncated() {
        assert_eq!(format_uptime(119), "1 minute"); // 1 min 59 sec -> 1 minute
        assert_eq!(format_uptime(3659), "1 hour"); // 1 hour 59 sec -> 1 hour
        assert_eq!(format_uptime(86459), "1 day"); // 1 day 59 sec -> 1 day
    }
}