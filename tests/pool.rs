use super::*;

#[test]
fn pool_is_pingable() {
    let pool = TestPool::spawn();

    let stratum_endpoint = pool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 10 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn pool_update_interval() {
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

// TODO:
// sending multiple configure at beginning is allowed for negotation
// sending submit before authorize fails
// sending authorize before subscribe fails
// unknown method return something?
//

#[tokio::test]
async fn stratum_happy_path() {
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
async fn stratum_some_errors() {
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

    let (subscribe, _, _) = client.subscribe().await.unwrap();

    assert_eq!(subscribe.subscriptions.len(), 2);

    assert!(client.authorize().await.is_ok());
}
