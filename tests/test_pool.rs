use super::*;

pub(crate) struct TestPool {
    bitcoind_handle: Bitcoind,
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

        let bitcoind_handle =
            Bitcoind::spawn(tempdir.clone(), bitcoind_port, rpc_port, zmq_port, false).unwrap();

        let pool_handle = CommandBuilder::new(format!(
            "pool 
                --chain signet
                --address 127.0.0.1 
                --port {pool_port} 
                --bitcoin-rpc-username satoshi
                --bitcoin-rpc-password nakamoto
                --bitcoin-rpc-port {rpc_port}
                --zmq-block-notifications tcp://127.0.0.1:{zmq_port}
                {}",
            args.to_args().join(" ")
        ))
        .capture_stderr(true)
        .capture_stdout(true)
        .env("RUST_LOG", "info")
        .integration_test(true)
        .spawn();

        for attempt in 0.. {
            match TcpStream::connect(format!("127.0.0.1:{pool_port}")) {
                Ok(_) => break,
                Err(_) if attempt < 100 => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!(
                    "Failed to connect to para pool after {} attempts: {}",
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

    pub(crate) async fn stratum_client(&self) -> stratum::Client {
        stratum::Client::connect(
            self.stratum_endpoint(),
            signet_username(),
            None,
            Duration::from_secs(1),
        )
        .await
        .unwrap()
    }

    #[allow(unused)]
    pub(crate) fn bitcoind_handle(&self) -> &Bitcoind {
        &self.bitcoind_handle
    }
}

impl Drop for TestPool {
    fn drop(&mut self) {
        self.bitcoind_handle.shutdown();
        self.pool_handle.kill().unwrap();
        self.pool_handle.wait().unwrap();
    }
}
