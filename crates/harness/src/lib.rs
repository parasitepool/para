use {
    anyhow::Error,
    bitcoin::{
        Address, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness,
        absolute::LockTime,
        blockdata::{opcodes::OP_TRUE, script::Builder},
        transaction::Version,
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

    bitcoind.mine_blocks(121).unwrap();

    let mut witness = Witness::new();
    witness.push(
        Builder::new()
            .push_opcode(OP_TRUE)
            .into_script()
            .into_bytes(),
    );

    // create 25 dependan transactions for every input
    let utxos = bitcoind.get_spendable_utxos().unwrap();

    println!("{:?}", utxos);

    for (outpoint, amount) in &utxos {
        let mut outpoint = *outpoint;
        for _ in 0..25 {
            let tx = Transaction {
                version: Version::TWO,
                lock_time: LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: outpoint,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                    witness: witness.clone(),
                }],
                output: vec![TxOut {
                    script_pubkey: bitcoind.op_true_address().script_pubkey(),
                    value: *amount,
                }],
            };

            outpoint = OutPoint {
                txid: tx.compute_txid(),
                vout: 0,
            };

            let result = bitcoind
                .client()
                .unwrap()
                .send_raw_transaction(&tx)
                .unwrap();

            println!("{:?}", result);
        }
    }

    let result = bitcoind.client().unwrap().get_mempool_info().unwrap();

    println!("Mempool size: {} bytes", result.bytes);
    println!("Mempool size: {} transactions", result.size);
    println!("Bitcoind rpc port: {}", bitcoind.rpc_port);

    while !SHUTTING_DOWN.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
    }
}
