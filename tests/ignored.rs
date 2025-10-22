use super::*;

/// These tests call some scripts that are not available in CI yet so marking them as ignored for
/// now.

#[test]
#[ignore]
fn miner() {
    let pool = TestPool::spawn();

    let bitcoind = pool.bitcoind_handle();

    bitcoind.mine_blocks(16).unwrap();

    let stratum_endpoint = pool.stratum_endpoint();

    let miner = CommandBuilder::new(format!(
        "miner --once --username {} {stratum_endpoint}",
        signet_username()
    ))
    .spawn();

    let stdout = miner.wait_with_output().unwrap();
    let output =
        serde_json::from_str::<Vec<Share>>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    assert_eq!(output.len(), 1);
}

#[test]
#[ignore]
fn concurrently_listening_workers_receive_new_templates_on_new_block() {
    let pool = TestPool::spawn();
    let endpoint = pool.stratum_endpoint();
    let user = signet_username();

    let gate = Arc::new(Barrier::new(3));
    let (out_1, in_1) = mpsc::channel();
    let (out_2, in_2) = mpsc::channel();

    thread::scope(|thread| {
        for out in [out_1.clone(), out_2.clone()].into_iter() {
            let gate = gate.clone();
            let endpoint = endpoint.clone();
            let user = user.clone();

            thread.spawn(move || {
                let mut template_watcher =
                    CommandBuilder::new(format!("template {endpoint} --username {user} --watch"))
                        .spawn();

                let mut reader = BufReader::new(template_watcher.stdout.take().unwrap());

                let initial_template = next_json::<Template>(&mut reader);

                gate.wait();

                let new_template = next_json::<Template>(&mut reader);

                out.send((initial_template, new_template)).ok();

                template_watcher.kill().unwrap();
                template_watcher.wait().unwrap();
            });
        }

        gate.wait();

        pool.bitcoind_handle().mine_blocks(1).unwrap();

        let (initial_template_worker_a, new_template_worker_a) =
            in_1.recv_timeout(Duration::from_secs(1)).unwrap();

        let (initial_template_worker_b, new_template_worker_b) =
            in_2.recv_timeout(Duration::from_secs(1)).unwrap();

        assert_eq!(
            initial_template_worker_a.prevhash,
            initial_template_worker_b.prevhash
        );

        assert_ne!(
            initial_template_worker_a.prevhash,
            new_template_worker_a.prevhash
        );

        assert_ne!(
            initial_template_worker_b.prevhash,
            new_template_worker_b.prevhash,
        );

        assert_eq!(
            new_template_worker_a.prevhash,
            new_template_worker_b.prevhash
        );

        assert!(new_template_worker_a.ntime >= initial_template_worker_a.ntime);
        assert!(new_template_worker_b.ntime >= initial_template_worker_b.ntime);
    });
}
