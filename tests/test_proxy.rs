use {
    super::*,
    api::proxy::Status,
    para::{USER_AGENT, stratum},
};

pub(crate) struct TestProxy {
    proxy_handle: Child,
    proxy_port: u16,
    api_port: u16,
}

fn allocate_ports() -> (u16, u16) {
    (
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
    )
}

fn build_proxy_command(
    upstream: &str,
    username: &str,
    proxy_port: u16,
    api_port: u16,
    args: impl ToArgs,
) -> CommandBuilder {
    CommandBuilder::new(format!(
        "proxy \
            --chain signet \
            --upstream {upstream} \
            --username {username} \
            --address 127.0.0.1 \
            --port {proxy_port} \
            --api-port {api_port} \
            {}",
        args.to_args().join(" ")
    ))
    .capture_stderr(true)
    .capture_stdout(true)
    .env("RUST_LOG", "info")
    .integration_test(true)
}

impl TestProxy {
    pub(crate) fn spawn_with_args(upstream: &str, username: &str, args: impl ToArgs) -> Self {
        let (proxy_port, api_port) = allocate_ports();

        let proxy_handle =
            build_proxy_command(upstream, username, proxy_port, api_port, args).spawn();

        for attempt in 0.. {
            match TcpStream::connect(format!("127.0.0.1:{api_port}")) {
                Ok(_) => break,
                Err(_) if attempt < 100 => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!(
                    "Failed to connect to proxy API after {} attempts: {}",
                    attempt, e
                ),
            }
        }

        Self {
            proxy_handle,
            proxy_port,
            api_port,
        }
    }

    pub(crate) fn spawn_expect_failure(
        upstream: &str,
        username: &str,
        args: impl ToArgs,
    ) -> String {
        let (proxy_port, api_port) = allocate_ports();

        let output = build_proxy_command(upstream, username, proxy_port, api_port, args)
            .spawn()
            .wait_with_output()
            .expect("Failed to wait for proxy process");

        assert!(
            !output.status.success(),
            "Expected proxy to fail but it succeeded"
        );

        String::from_utf8_lossy(&output.stderr).to_string()
    }

    pub(crate) fn stratum_endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.proxy_port)
    }

    pub(crate) fn stratum_client(&self) -> stratum::Client {
        self.stratum_client_for_username(&signet_username().to_string())
    }

    pub(crate) fn stratum_client_for_username(&self, username: &str) -> stratum::Client {
        let config = stratum::ClientConfig {
            address: self.stratum_endpoint(),
            username: Username::new(username),
            user_agent: USER_AGENT.into(),
            password: None,
            timeout: Duration::from_secs(5),
        };

        stratum::Client::new(config)
    }

    pub(crate) fn api_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.api_port)
    }

    pub(crate) async fn get_status(&self) -> reqwest::Result<Status> {
        let client = reqwest::Client::new();
        let url = format!("{}/proxy/status", self.api_endpoint());
        client.get(&url).send().await?.json().await
    }
}

impl Drop for TestProxy {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use nix::{
                sys::signal::{Signal, kill},
                unistd::Pid,
            };

            let pid = Pid::from_raw(self.proxy_handle.id() as i32);

            let _ = kill(pid, Signal::SIGTERM);

            for _ in 0..100 {
                match self.proxy_handle.try_wait() {
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

        let _ = self.proxy_handle.kill();
        let _ = self.proxy_handle.wait();
    }
}
