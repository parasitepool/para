use {
    anyhow::{Error, anyhow},
    arguments::Arguments,
    axum::{
        Router,
        http::{
            HeaderValue,
            header::{CONTENT_DISPOSITION, CONTENT_TYPE},
        },
    },
    clap::Parser,
    options::Options,
    std::{env, io, net::ToSocketAddrs, path::PathBuf, process},
    tokio::{runtime::Runtime, task},
    tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer},
};

mod arguments;
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
