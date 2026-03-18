use super::*;

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

async fn wait_for_exit(pool: &mut TestPool) {
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
    .expect("Pool did not exit");
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(30000)]
async fn exits_on_prolonged_bitcoind_failure() {
    let mut bitcoind = IsolatedBitcoind::spawn();
    let mut pool = TestPool::spawn_with_args(
        &bitcoind.bitcoind,
        "--bitcoind-timeout 5 --update-interval 1 --start-diff 0.00001",
    );

    bitcoind.shutdown();

    wait_for_exit(&mut pool).await;
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
