use super::*;

#[test]
fn ping_pool() {
    let pool = TestPool::spawn();

    let stratum_endpoint = pool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 10 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn mine_to_pool() {
    let pool = TestPool::spawn();

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
fn configure_template_update_interval() {
    let pool = TestPool::spawn_with_args("--update-interval 1");

    let stratum_endpoint = pool.stratum_endpoint();

    let output = CommandBuilder::new(format!(
        "template {stratum_endpoint} --username {}",
        signet_username()
    ))
    .spawn()
    .wait_with_output()
    .unwrap();

    let t1 = serde_json::from_str::<Template>(&String::from_utf8_lossy(&output.stdout)).unwrap();

    std::thread::sleep(Duration::from_secs(1));

    let output = CommandBuilder::new(format!(
        "template {stratum_endpoint} --username {}",
        signet_username()
    ))
    .spawn()
    .wait_with_output()
    .unwrap();

    let t2 = serde_json::from_str::<Template>(&String::from_utf8_lossy(&output.stdout)).unwrap();

    assert!(t1.ntime < t2.ntime);
}

#[test]
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

        CommandBuilder::new(format!(
            "miner --once --username {} {}",
            signet_username(),
            pool.stratum_endpoint()
        ))
        .spawn()
        .wait()
        .unwrap();

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

#[tokio::test]
async fn basic_initialization_flow() {
    let pool = TestPool::spawn();

    let mut client = pool.stratum_client().await;

    let (subscribe, _, _) = client.subscribe().await.unwrap();

    assert_eq!(subscribe.subscriptions.len(), 2);

    assert!(client.authorize().await.is_ok());

    let set_difficulty = match client.incoming.recv().await.unwrap() {
        Message::Notification { method: _, params } => {
            serde_json::from_value::<SetDifficulty>(params).unwrap()
        }
        _ => panic!(),
    };

    assert!(set_difficulty.difficulty() < Difficulty::from(0.01));

    let notify = match client.incoming.recv().await.unwrap() {
        Message::Notification { method: _, params } => {
            serde_json::from_value::<Notify>(params).unwrap()
        }
        _ => panic!(),
    };

    assert_eq!(notify.job_id, JobId::from(0));
    assert!(notify.clean_jobs);
}

#[tokio::test]
async fn configure_with_multiple_negotiation_steps() {
    let pool = TestPool::spawn();

    let mut client = pool.stratum_client().await;

    assert!(
        client
            .configure(vec!["unknown-extension".into()], None)
            .await
            .unwrap_err()
            .to_string()
            .contains("Unsupported extension")
    );

    assert!(
        client
            .configure(
                vec!["version-rolling".into()],
                Some(Version::from_str("1fffe000").unwrap())
            )
            .await
            .is_ok()
    );

    assert!(
        client
            .configure(
                vec!["version-rolling".into()],
                Some(Version::from_str("1fffe111").unwrap())
            )
            .await
            .is_ok()
    );

    let (subscribe, _, _) = client.subscribe().await.unwrap();

    assert_eq!(subscribe.subscriptions.len(), 2);

    assert!(client.authorize().await.is_ok());
}

#[tokio::test]
async fn authorize_before_subscribe_fails() {
    let pool = TestPool::spawn();

    let mut client = pool.stratum_client().await;

    assert!(
        client
            .authorize()
            .await
            .unwrap_err()
            .to_string()
            .contains("Method not allowed in current state")
    );
}

#[tokio::test]
async fn submit_before_authorize_fails() {
    let pool = TestPool::spawn();

    let mut client = pool.stratum_client().await;

    client.subscribe().await.unwrap();

    assert!(
        client
            .submit(
                JobId::new(3),
                Extranonce::random(8),
                Ntime::from(0),
                Nonce::from(12345),
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("Unauthorized")
    );
}
