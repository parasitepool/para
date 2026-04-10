use super::*;

pub(crate) struct TestPool {
    pool_handle: Child,
    pool_port: u16,
    http_port: u16,
    rpc_port: u16,
    _tempdir: Arc<TempDir>,
}

impl TestPool {
    pub(crate) fn spawn_on_port(bitcoind: &Bitcoind, port: u16, args: impl ToArgs) -> Self {
        Self::spawn_inner(bitcoind, Some(port), args)
    }

    pub(crate) fn spawn_with_args(bitcoind: &Bitcoind, args: impl ToArgs) -> Self {
        Self::spawn_inner(bitcoind, None, args)
    }

    fn spawn_inner(bitcoind: &Bitcoind, port: Option<u16>, args: impl ToArgs) -> Self {
        let tempdir = Arc::new(TempDir::new().unwrap());

        let rpc_port = bitcoind.rpc_port;
        let pool_port = port.unwrap_or_else(allocate_port);
        let http_port = allocate_port();
        let zmq_port = bitcoind.zmq_port;

        let pool_handle = CommandBuilder::new(format!(
            "pool
                --chain signet
                --address 127.0.0.1
                --port {pool_port}
                --http-port {http_port}
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
            pool_handle,
            pool_port,
            http_port,
            rpc_port,
            _tempdir: tempdir,
        }
    }

    pub(crate) fn pool_port(&self) -> u16 {
        self.pool_port
    }

    pub(crate) fn stratum_endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.pool_port)
    }

    pub(crate) fn api_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    pub(crate) async fn get_status(&self) -> reqwest::Result<api::PoolStatus> {
        reqwest::Client::new()
            .get(format!("{}/api/pool/status", self.api_endpoint()))
            .send()
            .await?
            .json()
            .await
    }

    pub(crate) async fn get_system_status(&self) -> reqwest::Result<api::SystemStatus> {
        reqwest::Client::new()
            .get(format!("{}/api/system/status", self.api_endpoint()))
            .send()
            .await?
            .json()
            .await
    }

    pub(crate) async fn get_user(&self, address: &str) -> reqwest::Result<UserDetail> {
        reqwest::Client::new()
            .get(format!("{}/api/pool/user/{}", self.api_endpoint(), address))
            .send()
            .await?
            .json()
            .await
    }

    pub(crate) async fn get_bitcoin_status(&self) -> reqwest::Result<api::BitcoinStatus> {
        reqwest::Client::new()
            .get(format!("{}/api/bitcoin/status", self.api_endpoint()))
            .send()
            .await?
            .json()
            .await
    }

    pub(crate) async fn wait_for_shares(
        &self,
        min_shares: u64,
        timeout: Duration,
    ) -> Result<api::PoolStatus, String> {
        let start = Instant::now();
        loop {
            if start.elapsed() > timeout {
                return Err(format!(
                    "Timeout waiting for {} shares after {:?}",
                    min_shares, timeout
                ));
            }

            match self.get_status().await {
                Ok(status) if status.stats.accepted_shares >= min_shares => return Ok(status),
                Ok(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    pub(crate) async fn wait_for_blocks(
        &self,
        min_blocks: u64,
        timeout: Duration,
    ) -> Result<api::PoolStatus, String> {
        let start = Instant::now();
        loop {
            if start.elapsed() > timeout {
                return Err(format!(
                    "Timeout waiting for {} blocks after {:?}",
                    min_blocks, timeout
                ));
            }

            match self.get_status().await {
                Ok(status) if status.block_count >= min_blocks => return Ok(status),
                Ok(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }

    pub(crate) async fn stratum_client(&self) -> stratum::client::Client {
        stratum::client::Client::new(
            self.stratum_endpoint(),
            signet_username(),
            None,
            USER_AGENT.into(),
            Duration::from_secs(1),
        )
    }

    pub(crate) async fn stratum_client_for_username(
        &self,
        username: &str,
    ) -> stratum::client::Client {
        stratum::client::Client::new(
            self.stratum_endpoint(),
            username.parse().unwrap(),
            None,
            USER_AGENT.into(),
            Duration::from_secs(1),
        )
    }

    pub(crate) fn bitcoind_rpc_port(&self) -> u16 {
        self.rpc_port
    }

    pub(crate) async fn get_block_height(&self) -> u64 {
        use bitcoind_async_client::{Auth, Client, traits::Reader};
        Client::new(
            format!("http://127.0.0.1:{}", self.rpc_port),
            Auth::UserPass("satoshi".into(), "nakamoto".into()),
            None,
            None,
            None,
        )
        .unwrap()
        .get_block_count()
        .await
        .unwrap()
    }

    pub(crate) async fn mine_block(&self) {
        let current_height = self.get_block_height().await;

        CommandBuilder::new(format!(
            "miner --mode block-found --username {} {}",
            signet_username(),
            self.stratum_endpoint()
        ))
        .spawn()
        .wait()
        .unwrap();

        for _ in 0..100 {
            if self.get_block_height().await > current_height {
                sleep(Duration::from_millis(100)).await;
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.pool_handle.try_wait()
    }
}

impl Drop for TestPool {
    fn drop(&mut self) {
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
        }

        let _ = self.pool_handle.kill();
        let _ = self.pool_handle.wait();
    }
}
