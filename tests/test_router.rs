use {
    super::*,
    api::{OrderDetail, OrderSummary, RouterStatus, UserDetail, UserSummary},
};

pub(crate) struct TestRouter {
    router_handle: Option<Child>,
    router_port: u16,
    http_port: u16,
    #[allow(unused)]
    tempdir: Arc<TempDir>,
}

impl TestRouter {
    pub(crate) fn spawn(descriptor: &str, bitcoind: &Bitcoind, args: impl ToArgs) -> Self {
        Self::spawn_with_probe_token(descriptor, bitcoind, args, None)
    }

    pub(crate) fn spawn_with_probe_token(
        descriptor: &str,
        bitcoind: &Bitcoind,
        args: impl ToArgs,
        probe_token: Option<&str>,
    ) -> Self {
        Self::launch(
            descriptor,
            bitcoind,
            args,
            Arc::new(TempDir::new().unwrap()),
            probe_token,
        )
    }

    pub(crate) fn restart(
        mut self,
        descriptor: &str,
        bitcoind: &Bitcoind,
        args: impl ToArgs,
    ) -> Self {
        self.terminate();
        Self::launch(descriptor, bitcoind, args, self.tempdir.clone(), None)
    }

    fn launch(
        descriptor: &str,
        bitcoind: &Bitcoind,
        args: impl ToArgs,
        tempdir: Arc<TempDir>,
        probe_token: Option<&str>,
    ) -> Self {
        let args = args.to_args();
        for attempt in 0..3 {
            match Self::try_launch(descriptor, bitcoind, &args, tempdir.clone(), probe_token) {
                Ok(router) => return router,
                Err(e) if attempt < 2 => {
                    eprintln!(
                        "router launch attempt {attempt} failed: {e}, retrying with new ports"
                    );
                }
                Err(e) => panic!("router failed to launch after 3 attempts: {e}"),
            }
        }
        unreachable!()
    }

    fn try_launch(
        descriptor: &str,
        bitcoind: &Bitcoind,
        args: &[String],
        tempdir: Arc<TempDir>,
        probe_token: Option<&str>,
    ) -> Result<Self, String> {
        let router_port = allocate_port();
        let http_port = allocate_port();

        let mut router_handle = CommandBuilder::new(format!(
            "router \
                --chain regtest \
                --address 127.0.0.1 \
                --port {router_port} \
                --http-port {http_port} \
                --bitcoin-rpc-username {} \
                --bitcoin-rpc-password {} \
                --bitcoin-rpc-port {} \
                --data-dir {} \
                --descriptor {descriptor} \
                {}",
            bitcoind.rpc_user,
            bitcoind.rpc_password,
            bitcoind.rpc_port,
            tempdir.path().to_str().unwrap(),
            args.join(" ")
        ))
        .capture_stderr(true)
        .capture_stdout(true)
        .env("RUST_LOG", "info")
        .integration_test(true)
        .spawn();

        // Drain pipes so the router can't wedge on a full buffer
        drain_pipe(router_handle.stdout.take());
        let stderr = drain_pipe(router_handle.stderr.take());

        const CONNECT_DEADLINE: Duration = Duration::from_secs(30);
        const SYNC_DEADLINE: Duration = Duration::from_secs(120);

        let started = Instant::now();
        loop {
            if let Ok(Some(status)) = router_handle.try_wait() {
                return Err(format!(
                    "router exited during startup with {status}.\nstderr:\n{}",
                    drained_output(&stderr)
                ));
            }

            if TcpStream::connect(format!("127.0.0.1:{http_port}")).is_ok() {
                break;
            }

            if started.elapsed() > CONNECT_DEADLINE {
                let _ = router_handle.kill();
                let _ = router_handle.wait();
                return Err(format!(
                    "router API not reachable within {CONNECT_DEADLINE:?}.\nstderr:\n{}",
                    drained_output(&stderr)
                ));
            }

            thread::sleep(Duration::from_millis(50));
        }

        let url = format!("http://127.0.0.1:{http_port}/api/router/status");

        let started = Instant::now();
        loop {
            if let Ok(Some(status)) = router_handle.try_wait() {
                return Err(format!(
                    "router exited while syncing wallet with {status}.\nstderr:\n{}",
                    drained_output(&stderr)
                ));
            }

            let url = url.clone();
            let probe_token = probe_token.map(str::to_string);
            let synced = thread::spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap();
                let mut request = client.get(url);
                if let Some(token) = probe_token {
                    request = request.bearer_auth(token);
                }
                request
                    .send()
                    .and_then(|response| response.json::<RouterStatus>())
                    .map(|status| status.wallet.synced)
                    .unwrap_or(false)
            })
            .join()
            .unwrap();

            if synced {
                break;
            }

            if started.elapsed() > SYNC_DEADLINE {
                let _ = router_handle.kill();
                let _ = router_handle.wait();
                panic!(
                    "Router wallet did not sync within {SYNC_DEADLINE:?}.\nstderr:\n{}",
                    drained_output(&stderr)
                );
            }

            thread::sleep(Duration::from_millis(100));
        }

        Ok(Self {
            router_handle: Some(router_handle),
            router_port,
            http_port,
            tempdir,
        })
    }

    pub(crate) fn stratum_endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.router_port)
    }

    pub(crate) fn api_endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    pub(crate) async fn get_status(&self) -> reqwest::Result<RouterStatus> {
        http_client()
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
        http_client()
            .post(format!("{}/api/router/order", self.api_endpoint()))
            .json(order)
            .send()
            .await
    }

    pub(crate) async fn get_order(&self, id: u32) -> reqwest::Result<OrderDetail> {
        http_client()
            .get(format!("{}/api/router/order/{id}", self.api_endpoint()))
            .send()
            .await?
            .json()
            .await
    }

    pub(crate) async fn get_users(&self) -> reqwest::Result<Vec<UserSummary>> {
        http_client()
            .get(format!("{}/api/router/users", self.api_endpoint()))
            .send()
            .await?
            .json()
            .await
    }

    pub(crate) async fn users_query(&self, query: &str) -> reqwest::Result<reqwest::Response> {
        let separator = if query.is_empty() { "" } else { "?" };
        http_client()
            .get(format!(
                "{}/api/router/users{separator}{query}",
                self.api_endpoint()
            ))
            .send()
            .await
    }

    pub(crate) async fn get_user(&self, address: &str) -> reqwest::Result<UserDetail> {
        http_client()
            .get(format!("{}/api/router/user/{address}", self.api_endpoint()))
            .send()
            .await?
            .json()
            .await
    }

    pub(crate) async fn list_orders(
        &self,
        address: Option<&str>,
    ) -> reqwest::Result<Vec<OrderSummary>> {
        let mut url = format!("{}/api/router/orders", self.api_endpoint());
        if let Some(addr) = address {
            url.push_str(&format!("?address={addr}"));
        }
        http_client().get(url).send().await?.json().await
    }

    pub(crate) async fn list_orders_query(
        &self,
        query: &str,
    ) -> reqwest::Result<reqwest::Response> {
        let separator = if query.is_empty() { "" } else { "?" };
        http_client()
            .get(format!(
                "{}/api/router/orders{separator}{query}",
                self.api_endpoint()
            ))
            .send()
            .await
    }

    pub(crate) async fn cancel_order(&self, id: u32) -> reqwest::Result<reqwest::Response> {
        http_client()
            .post(format!(
                "{}/api/router/order/{id}/cancel",
                self.api_endpoint()
            ))
            .send()
            .await
    }

    pub(crate) async fn clear_order(&self, id: u32) -> reqwest::Result<reqwest::Response> {
        http_client()
            .post(format!(
                "{}/api/router/order/{id}/clear",
                self.api_endpoint()
            ))
            .send()
            .await
    }

    pub(crate) async fn refund_order(
        &self,
        id: u32,
        request: &serde_json::Value,
    ) -> reqwest::Result<reqwest::Response> {
        http_client()
            .post(format!(
                "{}/api/router/order/{id}/refund",
                self.api_endpoint()
            ))
            .json(request)
            .send()
            .await
    }

    fn terminate(&mut self) {
        let Some(mut child) = self.router_handle.take() else {
            return;
        };

        #[cfg(unix)]
        {
            use nix::{
                sys::signal::{Signal, kill},
                unistd::Pid,
            };

            let pid = Pid::from_raw(child.id() as i32);

            let _ = kill(pid, Signal::SIGTERM);

            for _ in 0..100 {
                match child.try_wait() {
                    Ok(Some(_status)) => return,
                    Ok(None) => thread::sleep(Duration::from_millis(50)),
                    _ => break,
                }
            }
        }

        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for TestRouter {
    fn drop(&mut self) {
        self.terminate();
    }
}
