use {
    anyhow::Error,
    bitcoin::{
        Address, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
        absolute::LockTime,
        blockdata::{opcodes::OP_TRUE, script::Builder},
        transaction::Version,
    },
    bitcoincore_rpc::{Auth, Client, RpcApi},
    bitcoind::Bitcoind,
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
};

pub mod bitcoind;

type Result<T = (), E = Error> = std::result::Result<T, E>;

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

pub fn main() {
    ctrlc::set_handler(move || {
        if SHUTTING_DOWN.fetch_or(true, Ordering::Relaxed) {
            process::exit(1);
        }

        eprintln!("Shutting down");
    })
    .expect("Error setting <CTRL-C> handler");

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

    println!("Mining 101 blocks to get a mature output...");

    bitcoind.mine_blocks(101).unwrap();

    println!("Done creating 101 blocks");
    println!("Bitcoin rpc port: {}", bitcoind.rpc_port);
    println!("Bitcoin zmq port: {}", zmq_port);

    while !SHUTTING_DOWN.load(Ordering::Relaxed) {
        let result = bitcoind.client().unwrap().get_mempool_info().unwrap();
        println!("Mempool size: {} bytes", result.bytes);
        println!("Mempool size: {} transactions", result.size);
        println!("Bitcoin rpc port: {}", bitcoind.rpc_port);
        println!("Bitcoin zmq port: {}", zmq_port);

        if result.bytes < 5000000 {
            bitcoind.flood_mempool(Some(2)).unwrap();
        }

        std::thread::sleep(Duration::from_millis(5000));
    }
}
