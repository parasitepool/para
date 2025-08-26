#![cfg(all(target_os = "linux", feature = "ping-tests"))]
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
    bitcoind_handle: Child,
    ckpool_handle: Child,
    ckpool_port: u16,
    _tempdir: Arc<TempDir>,
}

impl TestCkpool {
    pub(crate) fn spawn() -> Self {
        let tempdir = Arc::new(TempDir::new().unwrap());

        let bitcoind_data_dir = tempdir.path().join("bitcoin");
        fs::create_dir(&bitcoind_data_dir).unwrap();

        let sockdir = tempdir.path().join("tmp");
        fs::create_dir(&sockdir).unwrap();

        let (bitcoind_port, rpc_port, zmq_port, ckpool_port) = (
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
        );

        let bitcoind_conf = tempdir.path().join("bitcoin.conf");

        fs::write(
            &bitcoind_conf,
            format!(
                "
signet=1
datadir={}

[signet]
# OP_TRUE
signetchallenge=51

server=1
txindex=1
zmqpubhashblock=tcp://127.0.0.1:{zmq_port}

port={bitcoind_port}

datacarriersize=100000
dbcache=8192
maxconnections=256
maxmempool=2048
mempoolfullrbf=1
minrelaytxfee=0.000001
par=-2

rpcbind=127.0.0.1
rpcport={rpc_port}
rpcallowip=127.0.0.1
rpcuser=foo
rpcpassword=bar
rpcthreads=8
rpcworkqueue=128
",
                &bitcoind_data_dir.display()
            ),
        )
        .unwrap();

        let bitcoind_handle = Command::new("bitcoind")
            .arg(format!("-conf={}", bitcoind_conf.display()))
            .stdout(Stdio::null())
            .spawn()
            .unwrap();

        let status = Command::new("bitcoin-cli")
            .args([
                &format!("-conf={}", bitcoind_conf.display()),
                "-rpcwait",
                "-rpcwaittimeout=5",
                "getblockchaininfo",
            ])
            .stdout(Stdio::null())
            .status()
            .unwrap();

        assert!(
            status.success(),
            "Failed to connect bitcoind after 5 seconds"
        );

        Lazy::force(&COMPILE_CKPOOL);

        let ckpool_conf = tempdir.path().join("ckpool.conf");

        fs::write(
            &ckpool_conf,
            format!(
                r#"{{
    "btcd" : [
        {{
            "url" : "127.0.0.1:{rpc_port}",
            "auth" : "foo",
            "pass" : "bar",
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
}}"#
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
            bitcoind_handle,
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
        self.bitcoind_handle.kill().unwrap();
        self.ckpool_handle.kill().unwrap();
        self.bitcoind_handle.wait().unwrap();
        self.ckpool_handle.wait().unwrap();
    }
}
