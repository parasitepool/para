use {super::*, para::subcommand::probe::Output};

#[test]
#[timeout(300000)]
#[ignore]
#[serial(heavy)]
fn probe_with_ckpool() {
    let bitcoind = bitcoind();
    let ckpool = TestCkpool::spawn(&bitcoind);

    let stratum_endpoint = ckpool.stratum_endpoint();

    let probe = CommandBuilder::new(format!(
        "probe {stratum_endpoint} --username {} --raw",
        signet_username()
    ))
    .spawn();

    let stdout = probe.wait_with_output().unwrap();
    let output = serde_json::from_str::<Notify>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    assert!(output.merkle_branches.is_empty());
    assert!(output.clean_jobs); // Initial job should have clean_jobs=true

    assert_eq!(stdout.status.code(), Some(0));

    let probe = CommandBuilder::new(format!(
        "probe {stratum_endpoint} --username {}",
        signet_username()
    ))
    .spawn();

    let stdout = probe.wait_with_output().unwrap();
    let output = serde_json::from_str::<Output>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    let dns = output.dns.unwrap();
    assert!(!dns.resolved.is_empty());
    assert!(dns.lookup_ms >= 0.0);

    let tcp = output.tcp.unwrap();
    assert_eq!(
        tcp.peer_address.port(),
        stratum_endpoint
            .rsplit(':')
            .next()
            .unwrap()
            .parse::<u16>()
            .unwrap()
    );
    assert!(tcp.connect_ms >= 0.0);

    let subscribe = output.subscribe.unwrap();
    assert!(subscribe.rtt_ms >= 0.0);
    assert!(!subscribe.enonce1.is_empty());
    assert!(subscribe.enonce2_size > 0);

    assert!(output.authorize.unwrap().rtt_ms >= 0.0);

    let notify = output.notify.unwrap();
    assert!(notify.wait_ms >= 0.0);
    assert!(notify.network_difficulty > Difficulty::from(0.00001));
    assert!(notify.clean_jobs);
    assert!(notify.ntime_human.contains('T'));
    assert!(notify.ntime_skew_seconds.abs() < 600);

    let coinbase = output.coinbase.unwrap();
    assert!(coinbase.size_bytes > 0);
    assert!(!coinbase.outputs.is_empty());

    assert_eq!(stdout.status.code(), Some(0));
}

#[tokio::test]
#[timeout(30000)]
async fn partial_report_on_stalled_server() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let stalled_server = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        sleep(Duration::from_secs(10)).await;
    });

    let stdout = CommandBuilder::new(format!(
        "probe 127.0.0.1:{port} --username {} --timeout 1",
        signet_username()
    ))
    .spawn()
    .wait_with_output()
    .unwrap();

    assert_eq!(stdout.status.code(), Some(1));

    let output = serde_json::from_str::<Output>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    assert!(output.dns.is_some());
    assert!(output.tcp.is_some());
    assert!(output.subscribe.is_none());
    assert!(output.authorize.is_none());
    assert!(output.notify.is_none());
    assert!(output.coinbase.is_none());

    stalled_server.abort();
}

#[tokio::test]
#[timeout(300000)]
#[ignore]
#[serial(heavy)]
async fn probe_watch_with_pool() {
    let bitcoind = bitcoind();
    let pool = TestPool::spawn_with_args(&bitcoind, "--start-diff 0.0001");

    let endpoint = pool.stratum_endpoint();
    let user = signet_username();

    let mut probe_watcher =
        CommandBuilder::new(format!("probe {endpoint} --username {user} --watch")).spawn();

    let mut reader = BufReader::new(probe_watcher.stdout.take().unwrap());

    let initial = next_json::<Output>(&mut reader);
    let initial_notify = initial.notify.unwrap();

    pool.mine_block().await;

    let updated = next_json::<Output>(&mut reader);
    let updated_notify = updated.notify.unwrap();

    assert_ne!(initial_notify.prevhash, updated_notify.prevhash);
    assert!(updated_notify.ntime >= initial_notify.ntime);
    assert!(updated.tcp.is_some());
    assert!(updated.subscribe.is_some());
    assert!(updated.coinbase.is_some());

    probe_watcher.kill().unwrap();
    probe_watcher.wait().unwrap();
}
