#[cfg(target_os = "linux")]
use {
    super::*,
    pgtemp::{PgTempDB, PgTempDBBuilder},
};

pub(crate) struct TestServer {
    child: Child,
    port: u16,
    tempdir: Arc<TempDir>,
    #[cfg(target_os = "linux")]
    pg_db: Option<PgTempDB>,
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
            #[cfg(target_os = "linux")]
            pg_db: None,
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) async fn spawn_with_sync_endpoint() -> Self {
        let psql_binpath = match Command::new("pg_config").arg("--bindir").output() {
            Ok(output) if output.status.success() => String::from_utf8(output.stdout)
                .ok()
                .map(|s| PathBuf::from(s.trim())),
            _ => None,
        };
        let pg_db = PgTempDB::from_builder(PgTempDBBuilder {
            temp_dir_prefix: None,
            db_user: None,
            password: None,
            port: None,
            dbname: None,
            persist_data_dir: false,
            dump_path: None,
            load_path: None,
            server_configs: Default::default(),
            bin_path: psql_binpath,
        });

        let database_url = pg_db.connection_uri();

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
            "server --address 127.0.0.1 --port {port} --log-dir {} --database-url {}",
            logdir.display(),
            database_url
        ))
        .integration_test(true)
        .spawn();

        for attempt in 0.. {
            if let Ok(response) = reqwest::get(format!("http://127.0.0.1:{port}")).await
                && response.status() == 200
            {
                break;
            }

            if attempt == 100 {
                panic!("Server did not respond to status check");
            }

            thread::sleep(Duration::from_millis(50));
        }

        Self {
            child,
            port,
            tempdir,
            pg_db: Some(pg_db),
        }
    }

    pub(crate) fn url(&self) -> Url {
        format!("http://127.0.0.1:{}", self.port).parse().unwrap()
    }

    pub(crate) fn log_dir(&self) -> PathBuf {
        self.tempdir.path().join("logs")
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn database_url(&self) -> Option<String> {
        self.pg_db.as_ref().map(|db| db.connection_uri())
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

    #[cfg(target_os = "linux")]
    pub(crate) async fn post_json<T: serde::Serialize, R: DeserializeOwned>(
        &self,
        path: impl AsRef<str>,
        body: &T,
    ) -> R {
        let client = reqwest::Client::new();
        let response = client
            .post(self.url().join(path.as_ref()).unwrap())
            .json(body)
            .send()
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "Response: {}",
            response.text().await.unwrap()
        );

        response.json().await.unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}
