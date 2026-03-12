use {super::*, once_cell::sync::Lazy};

static COMPILE_CKPOOL: Lazy<()> = Lazy::new(|| {
    stderr().write_all(b"compiling ckpool...\n").unwrap();
    stderr().flush().unwrap();

    let output = Command::new("bash")
        .arg("-c")
        .arg("cd ckpool && ./autogen.sh && ./configure && make")
        .output()
        .expect("ckpool build failed, try installing all dependencies first");

    if !output.status.success() {
        panic!(
            "ckpool build error: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    }

    stderr().write_all(b"compilation complete.\n").unwrap();
    stderr().flush().unwrap();
});

pub(crate) struct TestCkpool {
    ckpool_handle: Child,
    ckpool_port: u16,
    _tempdir: Arc<TempDir>,
}

impl TestCkpool {
    pub(crate) fn spawn(bitcoind: &Bitcoind) -> Self {
        let tempdir = Arc::new(TempDir::new().unwrap());

        let sockdir = tempdir.path().join("tmp");
        fs::create_dir(&sockdir).unwrap();

        Lazy::force(&COMPILE_CKPOOL);

        let ckpool_port = allocate_port();
        let zmq_port = bitcoind.zmq_port.expect("bitcoind missing zmq_port");

        let ckpool_conf = tempdir.path().join("ckpool.conf");

        fs::write(
            &ckpool_conf,
            format!(
                r#"{{
    "btcd" : [
        {{
            "url" : "127.0.0.1:{}",
            "auth" : "satoshi",
            "pass" : "nakamoto",
            "notify" : true
        }}
    ],
    "serverurl" : [
        "127.0.0.1:{ckpool_port}"
    ],
    "btcaddress" : "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc",
    "btcsig" : "|parasite|",
    "blockpoll" : 10,
    "donation" : 2.0,
    "nonce1length" : 4,
    "nonce2length" : 8,
    "update_interval" : 10,
    "version_mask" : "1fffe000",
    "mindiff" : 1,
    "startdiff" : 1,
    "maxdiff" : 0,
    "zmqblock" : "tcp://127.0.0.1:{zmq_port}",
    "logdir" : "logs"
}}"#,
                bitcoind.rpc_port,
            ),
        )
        .unwrap();

        let ckpool_handle = Command::new("./ckpool/src/ckpool")
            .arg("-B")
            .arg("--config")
            .arg(format!("{}", ckpool_conf.display()))
            .arg("--sockdir")
            .arg(format!("{}", sockdir.display()))
            .arg("--loglevel")
            .arg("7")
            .arg("--signet")
            .arg("--log-txns")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        for attempt in 0.. {
            match TcpStream::connect(format!("127.0.0.1:{ckpool_port}")) {
                Ok(_) => break,
                Err(_) if attempt < 100 => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!(
                    "Failed to connect to ckpool after {} attempts: {}",
                    attempt, e
                ),
            }
        }

        Self {
            ckpool_handle,
            ckpool_port,
            _tempdir: tempdir,
        }
    }

    pub(crate) fn stratum_endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.ckpool_port)
    }
}

impl Drop for TestCkpool {
    fn drop(&mut self) {
        self.ckpool_handle.kill().unwrap();
        self.ckpool_handle.wait().unwrap();
    }
}
