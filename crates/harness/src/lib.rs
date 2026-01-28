use {
    anyhow::Error,
    bitcoin::{
        Address, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
        absolute::LockTime,
        blockdata::{opcodes::OP_TRUE, script::Builder},
        hashes::{Hash, HashEngine, Hmac, hmac, sha256},
        transaction::Version,
    },
    bitcoind::Bitcoind,
    bitcoind_async_client::{
        Auth, Client,
        traits::{Broadcaster, Reader},
    },
    cargo_metadata::MetadataCommand,
    clap::{Parser, Subcommand},
    rand::{RngCore, rng},
    serde::{Deserialize, Serialize},
    serde_json::json,
    std::{
        fs,
        net::TcpListener,
        path::PathBuf,
        process::{self, Child, Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    },
    tempfile::TempDir,
    tokio::{runtime::Runtime, time::sleep},
};

pub mod bitcoind;

type Result<T = (), E = Error> = std::result::Result<T, E>;

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

fn workspace_root() -> String {
    MetadataCommand::new()
        .no_deps()
        .exec()
        .expect("cargo metadata")
        .workspace_root
        .into_std_path_buf()
        .display()
        .to_string()
}

#[derive(Parser)]
#[command(name = "harness")]
#[command(about = "Bitcoin testing harness for flooding mempool and spawning nodes")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    Spawn,
    Flood {
        #[arg(long, default_value = "38332")]
        rpc_port: u16,
        #[arg(long, default_value = "satoshi")]
        rpc_user: String,
        #[arg(long, default_value = "nakamoto")]
        rpc_password: String,
        #[arg(long, default_value = "signet")]
        network: String,
        #[arg(long)]
        breadth: Option<u64>,
        #[arg(long)]
        continuous: Option<u64>,
    },
}

fn parse_network(s: &str) -> Network {
    match s.to_lowercase().as_str() {
        "mainnet" | "main" => Network::Bitcoin,
        "testnet" | "test" => Network::Testnet,
        "signet" => Network::Signet,
        "regtest" => Network::Regtest,
        _ => panic!("Unknown network: {}", s),
    }
}

pub fn main() {
    let cli = Cli::parse();

    ctrlc::set_handler(move || {
        if SHUTTING_DOWN.fetch_or(true, Ordering::Relaxed) {
            process::exit(1);
        }

        eprintln!("\nShutting down...");
    })
    .expect("Error setting <CTRL-C> handler");

    Runtime::new()
        .expect("Failed to create tokio runtime")
        .block_on(async {
            match cli.command {
                Some(Commands::Flood {
                    rpc_port,
                    rpc_user,
                    rpc_password,
                    network,
                    breadth,
                    continuous,
                }) => {
                    let network = parse_network(&network);
                    let bitcoind = Bitcoind::connect(rpc_port, rpc_user, rpc_password, network)
                        .await
                        .expect("Failed to connect to bitcoind");

                    if let Some(target_bytes) = continuous {
                        println!(
                            "Running in continuous mode, target mempool size: {} bytes",
                            target_bytes
                        );
                        while !SHUTTING_DOWN.load(Ordering::Relaxed) {
                            let mempool_info =
                                bitcoind.client().unwrap().get_mempool_info().await.unwrap();
                            println!(
                                "Mempool: {} txs, {} bytes",
                                mempool_info.size, mempool_info.bytes
                            );

                            if (mempool_info.bytes as u64) < target_bytes {
                                match bitcoind.flood_mempool(breadth).await {
                                    Ok(count) => println!("Created {} transactions", count),
                                    Err(e) => eprintln!("Error flooding mempool: {}", e),
                                }
                            }

                            std::thread::sleep(Duration::from_secs(5));
                        }
                    } else {
                        match bitcoind.flood_mempool(breadth).await {
                            Ok(count) => println!("Created {} transactions", count),
                            Err(e) => {
                                eprintln!("Error: {}", e);
                                process::exit(1);
                            }
                        }
                    }
                }

                Some(Commands::Spawn) | None => {
                    run_ephemeral_harness().await;
                }
            }
        });
}

async fn run_ephemeral_harness() {
    let tempdir = Arc::new(TempDir::new().unwrap());

    let (bitcoind_port, rpc_port, zmq_port) = (
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port(),
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port(),
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port(),
    );

    let bitcoind =
        Bitcoind::spawn(tempdir.clone(), bitcoind_port, rpc_port, zmq_port, true).unwrap();

    println!("Bitcoin rpc port: {}", bitcoind.rpc_port);
    println!("Bitcoin zmq port: {}", zmq_port);

    while !SHUTTING_DOWN.load(Ordering::Relaxed) {
        let result = bitcoind.client().unwrap().get_mempool_info().await.unwrap();
        println!("Mempool size: {} bytes", result.bytes);
        println!("Mempool size: {} transactions", result.size);
        println!("Bitcoin rpc port: {}", bitcoind.rpc_port);
        println!("Bitcoin zmq port: {}", zmq_port);

        if result.bytes < 5000000 {
            let _ = bitcoind.flood_mempool(Some(2)).await;
        }

        sleep(Duration::from_millis(5000)).await;
    }
}
