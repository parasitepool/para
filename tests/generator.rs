use super::*;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct IsolatedBitcoind {
    bitcoind: Bitcoind,
    tempdir: Arc<TempDir>,
    bitcoind_port: u16,
    rpc_port: u16,
    zmq_port: u16,
}

impl IsolatedBitcoind {
    fn spawn() -> Self {
        let tempdir = Arc::new(TempDir::new().unwrap());
        let bitcoind_port = allocate_port();
        let rpc_port = allocate_port();
        let zmq_port = allocate_port();

        let bitcoind =
            Bitcoind::spawn(tempdir.clone(), bitcoind_port, rpc_port, zmq_port, false).unwrap();

        Self {
            bitcoind,
            tempdir,
            bitcoind_port,
            rpc_port,
            zmq_port,
        }
    }

    fn shutdown(&mut self) {
        self.bitcoind.shutdown();
    }

    fn restart(&mut self) {
        self.bitcoind = Bitcoind::spawn(
            self.tempdir.clone(),
            self.bitcoind_port,
            self.rpc_port,
            self.zmq_port,
            false,
        )
        .unwrap();
    }
}

struct MockBitcoindRpc {
    handle: Option<JoinHandle<()>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl MockBitcoindRpc {
    async fn spawn(port: u16, template: serde_json::Value) -> Self {
        let listener = bind_rpc_listener(port).await;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let template = Arc::new(template);

        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        let (stream, _) = accept.unwrap();
                        let template = template.clone();
                        tokio::spawn(async move {
                            handle_rpc_connection(stream, template).await;
                        });
                    }
                }
            }
        });

        Self {
            handle: Some(handle),
            shutdown_tx: Some(shutdown_tx),
        }
    }

    async fn shutdown(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }

        if let Some(handle) = self.handle.take() {
            handle.await.unwrap();
        }
    }
}

impl Drop for MockBitcoindRpc {
    fn drop(&mut self) {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }

        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

async fn bind_rpc_listener(port: u16) -> TcpListener {
    for attempt in 0.. {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return listener,
            Err(_) if attempt < 100 => sleep(Duration::from_millis(50)).await,
            Err(err) => panic!("Failed to bind mock RPC listener on port {port}: {err}"),
        }
    }

    unreachable!()
}

async fn handle_rpc_connection(
    mut stream: tokio::net::TcpStream,
    template: Arc<serde_json::Value>,
) {
    let request = read_http_request(&mut stream).await;
    let id = request
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let method = request
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    let body = (match method {
        "getblocktemplate" => serde_json::json!({
            "result": template.as_ref(),
            "error": null,
            "id": id,
        }),
        _ => serde_json::json!({
            "result": null,
            "error": {
                "code": -32601,
                "message": format!("unsupported method {method}"),
            },
            "id": id,
        }),
    })
    .to_string();

    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body,
    );

    stream.write_all(response.as_bytes()).await.unwrap();
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
    let mut buffer = Vec::new();

    let headers_end = loop {
        let mut chunk = [0; 4096];
        let bytes_read = stream.read(&mut chunk).await.unwrap();

        assert_ne!(bytes_read, 0, "Mock RPC client disconnected");

        buffer.extend_from_slice(&chunk[..bytes_read]);

        if let Some(headers_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break headers_end;
        }
    };

    let headers = std::str::from_utf8(&buffer[..headers_end]).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let line = line.trim();
            let (name, value) = line.split_once(':')?;
            (name.eq_ignore_ascii_case("content-length"))
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);

    let body_start = headers_end + 4;

    while buffer.len() < body_start + content_length {
        let mut chunk = [0; 4096];
        let bytes_read = stream.read(&mut chunk).await.unwrap();

        assert_ne!(bytes_read, 0, "Mock RPC client disconnected");

        buffer.extend_from_slice(&chunk[..bytes_read]);
    }

    serde_json::from_slice(&buffer[body_start..body_start + content_length]).unwrap()
}

async fn get_block_template(bitcoind: &Bitcoind) -> serde_json::Value {
    bitcoind
        .client()
        .unwrap()
        .call_raw(
            "getblocktemplate",
            &[serde_json::json!({
                "capabilities": ["coinbasetxn", "workid", "coinbase/append"],
                "rules": ["segwit", "signet"],
            })],
        )
        .await
        .unwrap()
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(30000)]
async fn exits_on_prolonged_rpc_failure() {
    let mut bitcoind = IsolatedBitcoind::spawn();
    let mut pool = TestPool::spawn_with_args(
        &bitcoind.bitcoind,
        "--bitcoind-timeout 5 --update-interval 1 --start-diff 0.00001",
    );

    bitcoind.shutdown();

    timeout(Duration::from_secs(20), async {
        loop {
            match pool.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => sleep(Duration::from_millis(200)).await,
                Err(e) => panic!("Failed to wait for pool: {e}"),
            }
        }
    })
    .await
    .expect("Pool did not exit after bitcoind shutdown");
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(30000)]
async fn exits_on_prolonged_zmq_failure() {
    let mut bitcoind = IsolatedBitcoind::spawn();
    let template = get_block_template(&bitcoind.bitcoind).await;
    let mut pool = TestPool::spawn_with_args(
        &bitcoind.bitcoind,
        "--bitcoind-timeout 5 --update-interval 1 --start-diff 0.00001",
    );

    bitcoind.shutdown();

    let mut rpc = MockBitcoindRpc::spawn(bitcoind.rpc_port, template).await;

    timeout(Duration::from_secs(20), async {
        loop {
            match pool.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => sleep(Duration::from_millis(200)).await,
                Err(e) => panic!("Failed to wait for pool: {e}"),
            }
        }
    })
    .await
    .expect("Pool did not exit after ZMQ shutdown");

    rpc.shutdown().await;
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(60000)]
async fn zmq_reconnects_after_bitcoind_restart() {
    let mut bitcoind = IsolatedBitcoind::spawn();
    let mut pool = TestPool::spawn_with_args(
        &bitcoind.bitcoind,
        "--bitcoind-timeout 30 --update-interval 1 --start-diff 0.00001",
    );

    bitcoind.shutdown();

    sleep(Duration::from_secs(3)).await;

    bitcoind.restart();

    sleep(Duration::from_secs(5)).await;

    assert!(
        pool.try_wait().unwrap().is_none(),
        "Pool should still be running after bitcoind restart"
    );
}
