use super::*;

pub(crate) fn spawn(
    tempdir: Arc<TempDir>,
    bitcoind_port: u16,
    rpc_port: u16,
    zmq_port: u16,
) -> Child {
    let bitcoind_data_dir = tempdir.path().join("bitcoin");
    fs::create_dir(&bitcoind_data_dir).unwrap();

    let bitcoind_conf = bitcoind_data_dir.join("bitcoin.conf");

    fs::write(
        &bitcoind_conf,
        format!(
            "
signet=1
datadir={}

[signet]
# OP_TRUE
signetchallenge=51

server=1
txindex=1
zmqpubhashblock=tcp://127.0.0.1:{zmq_port}

port={bitcoind_port}

blockmaxweight=3900000
blockreconstructionextratxn=1000
checkblocks=6
datacarriersize=100000
maxconnections=256
maxmempool=2048
mempoolfullrbf=1
minrelaytxfee=0.000001

rpcbind=127.0.0.1
rpcport={rpc_port}
rpcallowip=127.0.0.1
rpcuser=foo
rpcpassword=bar
",
            &bitcoind_data_dir.display()
        ),
    )
    .unwrap();

    let bitcoind_handle = Command::new("bitcoind")
        .arg(format!("-conf={}", bitcoind_conf.display()))
        .stdout(Stdio::null())
        .spawn()
        .unwrap();

    let status = Command::new("bitcoin-cli")
        .args([
            &format!("-conf={}", bitcoind_conf.display()),
            "-rpcwait",
            "-rpcwaittimeout=5",
            "getblockchaininfo",
        ])
        .stdout(Stdio::null())
        .status()
        .unwrap();

    assert!(
        status.success(),
        "Failed to connect bitcoind after 5 seconds"
    );

    bitcoind_handle
}
