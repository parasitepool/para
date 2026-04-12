use {
    super::*,
    api::{OrderDetail, RouterStatus},
};

pub(crate) struct TestRouter {
    router_handle: Child,
    router_port: u16,
    http_port: u16,
}

impl TestRouter {
    pub(crate) fn spawn(descriptor: &str, bitcoind: &Bitcoind, args: impl ToArgs) -> Self {
        let router_port = allocate_port();
        let http_port = allocate_port();

        let router_handle = CommandBuilder::new(format!(
            "router \
                --chain regtest \
                --address 127.0.0.1 \
                --port {router_port} \
                --http-port {http_port} \
                --bitcoin-rpc-username {} \
                --bitcoin-rpc-password {} \
                --bitcoin-rpc-port {} \
                --descriptor {descriptor} \
                {}",
            bitcoind.rpc_user,
            bitcoind.rpc_password,
            bitcoind.rpc_port,
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

    pub(crate) async fn get_status(&self) -> reqwest::Result<RouterStatus> {
        reqwest::Client::new()
            .get(format!("{}/api/router/status", self.api_endpoint()))
            .send()
            .await?
            .json()
            .await
    }

    pub(crate) async fn add_order(
        &self,
        order: &api::OrderRequest,
    ) -> reqwest::Result<reqwest::Response> {
        reqwest::Client::new()
            .post(format!("{}/api/router/order", self.api_endpoint()))
            .json(order)
            .send()
            .await
    }

    pub(crate) async fn get_order(&self, id: u32) -> reqwest::Result<OrderDetail> {
        reqwest::Client::new()
            .get(format!("{}/api/router/order/{id}", self.api_endpoint()))
            .send()
            .await?
            .json()
            .await
    }

    pub(crate) async fn list_orders(&self, address: Option<&str>) -> reqwest::Result<Vec<u32>> {
        let mut url = format!("{}/api/router/orders", self.api_endpoint());
        if let Some(addr) = address {
            url.push_str(&format!("?address={addr}"));
        }
        reqwest::Client::new().get(url).send().await?.json().await
    }

    pub(crate) async fn cancel_order(&self, id: u32) -> reqwest::Result<reqwest::Response> {
        reqwest::Client::new()
            .post(format!(
                "{}/api/router/order/{id}/cancel",
                self.api_endpoint()
            ))
            .send()
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
