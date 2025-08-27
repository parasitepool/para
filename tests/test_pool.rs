use super::*;

// TODO: dedup bitcoind with test_ckpool.rs
pub(crate) struct TestPool {
    bitcoind_handle: Child,
    pool_handle: Child,
    pool_port: u16,
    _tempdir: Arc<TempDir>,
}

impl TestPool {
    pub(crate) fn spawn() -> Self {
        Self::spawn_with_args("")
    }

    pub(crate) fn spawn_with_args(args: impl ToArgs) -> Self {
        let tempdir = Arc::new(TempDir::new().unwrap());

        let bitcoind_data_dir = tempdir.path().join("bitcoin");
        fs::create_dir(&bitcoind_data_dir).unwrap();

        let (bitcoind_port, rpc_port, zmq_port, pool_port) = (
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
            TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port(),
        );

        let bitcoind_conf = tempdir.path().join("bitcoin.conf");

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

        let pool_handle = CommandBuilder::new(format!(
            "pool 
                --chain signet
                --address 127.0.0.1 
                --port {pool_port} 
                --bitcoin-rpc-username foo 
                --bitcoin-rpc-password bar 
                --bitcoin-rpc-port {rpc_port} 
                {}",
            args.to_args().join(" ")
        ))
        .integration_test(true)
        .spawn();

        for attempt in 0.. {
            match TcpStream::connect(format!("127.0.0.1:{pool_port}")) {
                Ok(_) => break,
                Err(_) if attempt < 100 => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!(
                    "Failed to connect to ckpool after {} attempts: {}",
                    attempt, e
                ),
            }
        }

        Self {
            bitcoind_handle,
            pool_handle,
            pool_port,
            _tempdir: tempdir,
        }
    }

    pub(crate) fn stratum_endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.pool_port)
    }
}

impl Drop for TestPool {
    fn drop(&mut self) {
        self.bitcoind_handle.kill().unwrap();
        self.pool_handle.kill().unwrap();
        self.bitcoind_handle.wait().unwrap();
        self.pool_handle.wait().unwrap();
    }
}
