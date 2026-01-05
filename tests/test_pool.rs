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
        let config = stratum::ClientConfig {
            address: self.stratum_endpoint(),
            username: signet_username(),
            user_agent: USER_AGENT.into(),
            password: None,
            timeout: Duration::from_secs(1),
        };

        stratum::Client::new(config)
    }

    pub(crate) async fn stratum_client_for_username(&self, username: &str) -> stratum::Client {
        let config = stratum::ClientConfig {
            address: self.stratum_endpoint(),
            username: Username::new(username),
            user_agent: USER_AGENT.into(),
            password: None,
            timeout: Duration::from_secs(1),
        };

        stratum::Client::new(config)
    }

    #[allow(unused)]
    pub(crate) fn bitcoind_handle(&self) -> &Bitcoind {
        &self.bitcoind_handle
    }

    pub(crate) fn get_block_height(&self) -> u64 {
        self.bitcoind_handle
            .client()
            .unwrap()
            .get_block_count()
            .unwrap()
    }

    pub(crate) fn mine_block(&self) {
        let current_height = self.get_block_height();

        CommandBuilder::new(format!(
            "miner --mode block-found --username {} {}",
            signet_username(),
            self.stratum_endpoint()
        ))
        .spawn()
        .wait()
        .unwrap();

        for _ in 0..100 {
            if self.get_block_height() > current_height {
                thread::sleep(Duration::from_millis(500));
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for TestPool {
    fn drop(&mut self) {
        self.bitcoind_handle.shutdown();
        #[cfg(unix)]
        {
            use nix::{
                sys::signal::{Signal, kill},
                unistd::Pid,
            };

            let pid = Pid::from_raw(self.pool_handle.id() as i32);

            let _ = kill(pid, Signal::SIGTERM);

            for _ in 0..100 {
                match self.pool_handle.try_wait() {
                    Ok(Some(_status)) => {
                        return;
                    }
                    Ok(None) => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    _ => break,
                }
            }

            let _ = self.pool_handle.kill();
            let _ = self.pool_handle.wait();
        }

        #[cfg(not(unix))]
        {
            let _ = self.pool_handle.kill();
            let _ = self.pool_handle.wait();
        }
    }
}
