use {super::*, api::RouterStatus};

pub(crate) struct TestRouter {
    router_handle: Child,
    router_port: u16,
    http_port: u16,
}

impl TestRouter {
    pub(crate) fn spawn(
        upstreams: &[(&str, &str)],
        bitcoind_rpc_port: u16,
        args: impl ToArgs,
    ) -> Self {
        let router_port = allocate_port();
        let http_port = allocate_port();

        let upstream_args: Vec<String> = upstreams
            .iter()
            .map(|(username, endpoint)| format!("--upstream {username}@{endpoint}"))
            .collect();

        let router_handle = CommandBuilder::new(format!(
            "router \
                --chain signet \
                --address 127.0.0.1 \
                --port {router_port} \
                --http-port {http_port} \
                --bitcoin-rpc-username satoshi \
                --bitcoin-rpc-password nakamoto \
                --bitcoin-rpc-port {bitcoind_rpc_port} \
                {} \
                {}",
            upstream_args.join(" "),
            args.to_args().join(" ")
        ))
        .capture_stderr(true)
        .capture_stdout(true)
        .env("RUST_LOG", "info")
        .integration_test(true)
        .spawn();

        for attempt in 0.. {
            match TcpStream::connect(format!("127.0.0.1:{http_port}")) {
                Ok(_) => break,
                Err(_) if attempt < 100 => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!(
                    "Failed to connect to router API after {} attempts: {}",
                    attempt, e
                ),
            }
        }

        Self {
            router_handle,
            router_port,
            http_port,
        }
    }

    pub(crate) fn stratum_endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.router_port)
    }

    pub(crate) fn api_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.router_handle.try_wait()
    }

    pub(crate) async fn get_status(&self) -> reqwest::Result<RouterStatus> {
        reqwest::Client::new()
            .get(format!("{}/api/router/status", self.api_endpoint()))
            .send()
            .await?
            .json()
            .await
    }
}

impl Drop for TestRouter {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use nix::{
                sys::signal::{Signal, kill},
                unistd::Pid,
            };

            let pid = Pid::from_raw(self.router_handle.id() as i32);

            let _ = kill(pid, Signal::SIGTERM);

            for _ in 0..100 {
                match self.router_handle.try_wait() {
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

        let _ = self.router_handle.kill();
        let _ = self.router_handle.wait();
    }
}
