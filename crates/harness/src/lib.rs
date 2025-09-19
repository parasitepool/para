use {
    anyhow::Error,
    bitcoin::{
        Address, Network, OutPoint, ScriptBuf, blockdata::opcodes::OP_TRUE,
        blockdata::script::Builder,
    },
    bitcoincore_rpc::{Auth, Client, RpcApi, json::ScanTxOutRequest},
    bitcoind::Bitcoind,
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

// const COIN_VALUE: u64 = 100_000_000;
const MATURITY: u64 = 100;

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

    let bitcoind = Bitcoind::spawn(tempdir.clone(), bitcoind_port, rpc_port, zmq_port).unwrap();

    bitcoind.mine_blocks(110).unwrap();

    println!("{:?}", bitcoind.get_spendable_utxos().unwrap());

    while !SHUTTING_DOWN.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
    }
}
