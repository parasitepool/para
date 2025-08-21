use super::*;

pub(crate) struct TestServer {
    child: Child,
    port: u16,
    tempdir: Arc<TempDir>,
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

        let tempdir = Arc::new(TempDir::new().unwrap());
        let logdir = tempdir.path().join("logs");
        fs::create_dir(&logdir).unwrap();
        fs::create_dir(logdir.join("pool")).unwrap();
        fs::create_dir(logdir.join("users")).unwrap();

        let child = CommandBuilder::new(format!(
            "server --address 127.0.0.1 --port {port} --log-dir {} {}",
            logdir.display(),
            args.to_args().join(" ")
        ))
        .capture_stderr(false)
        .capture_stdout(false)
        .integration_test(true)
        .spawn();

        for attempt in 0.. {
            if let Ok(response) = reqwest::blocking::get(format!("http://127.0.0.1:{port}"))
                && response.status() == 200
            {
                break;
            }

            if attempt == 100 {
                panic!("Server did not respond to status check",);
            }

            thread::sleep(Duration::from_millis(50));
        }

        Self {
            child,
            port,
            tempdir,
        }
    }

    pub(crate) fn url(&self) -> Url {
        format!("http://127.0.0.1:{}", self.port).parse().unwrap()
    }

    pub(crate) fn log_dir(&self) -> PathBuf {
        self.tempdir.path().join("logs")
    }

    #[track_caller]
    pub(crate) fn assert_response(&self, path: impl AsRef<str>, expected_response: &str) {
        let response = reqwest::blocking::get(self.url().join(path.as_ref()).unwrap()).unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            response.text().unwrap()
        );

        pretty_assert_eq!(response.text().unwrap(), expected_response);
    }

    #[track_caller]
    pub(crate) fn get_json<T: DeserializeOwned>(&self, path: impl AsRef<str>) -> T {
        let request = reqwest::blocking::Client::new()
            .get(self.url().join(path.as_ref()).unwrap())
            .header(reqwest::header::ACCEPT, "application/json");

        let response = request.send().unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            response.text().unwrap()
        );

        response.json().unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}
