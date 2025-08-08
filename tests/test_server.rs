use super::*;

pub(crate) struct TestServer {
    child: Child,
    #[allow(unused)]
    port: u16,
}

impl TestServer {
    pub(crate) fn spawn() -> Self {
        Self::spawn_with_args("")
    }

    pub(crate) fn spawn_with_args(args: impl ToArgs) -> Self {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();

        let child = CommandBuilder::new(format!(
            "server --address 127.0.0.1 --port {port} {}",
            args.to_args().join(" ")
        ))
        .integration_test(true)
        .spawn();

        for attempt in 0.. {
            if let Ok(response) = reqwest::blocking::get(format!("http://127.0.0.1:{port}")) {
                if response.status() == 200 {
                    break;
                }
            }

            if attempt == 100 {
                panic!("Server did not respond to status check",);
            }

            thread::sleep(Duration::from_millis(50));
        }

        Self { child, port }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}
