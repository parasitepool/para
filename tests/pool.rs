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
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");

    let stratum_endpoint = pool.stratum_endpoint();

    let miner = CommandBuilder::new(format!(
        "miner --mode share-found --username {} {stratum_endpoint} --cpu-cores 1",
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
    let pool = TestPool::spawn_with_args("--update-interval 1 --start-diff 0.00001");

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

#[tokio::test]
async fn basic_initialization_flow() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");

    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();

    assert_eq!(subscribe.subscriptions.len(), 2);

    assert!(client.authorize().await.is_ok());

    let difficulty = match events.recv().await.unwrap() {
        stratum::Event::SetDifficulty(difficulty) => difficulty,
        _ => panic!("Expected SetDifficulty"),
    };

    assert_eq!(difficulty, Difficulty::from(0.00001));

    let notify = match events.recv().await.unwrap() {
        stratum::Event::Notify(n) => n,
        _ => panic!("Expected Notify"),
    };

    assert_eq!(notify.job_id, JobId::from(0));
    assert!(notify.clean_jobs);
}

#[tokio::test]
async fn configure_with_multiple_negotiation_steps() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");

    let client = pool.stratum_client().await;
    let _ = client.connect().await.unwrap();

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

    let client = pool.stratum_client().await;
    let _ = client.connect().await.unwrap();

    assert!(
        client
            .authorize()
            .await
            .unwrap_err()
            .to_string()
            .contains("Method not allowed")
    );
}

#[tokio::test]
async fn submit_before_authorize_fails() {
    let pool = TestPool::spawn();

    let client = pool.stratum_client().await;
    let _ = client.connect().await.unwrap();

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

#[tokio::test]
async fn duplicate_share_rejected() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");
    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();
    let extranonce1 = subscribe.extranonce1;
    let extranonce2 = Extranonce::random(subscribe.extranonce2_size);

    client.authorize().await.unwrap();

    let mut difficulty = stratum::Difficulty::from(1);

    let notify = loop {
        match events.recv().await.unwrap() {
            stratum::Event::SetDifficulty(diff) => difficulty = diff,
            stratum::Event::Notify(notify) => break notify,
            _ => {}
        }
    };

    let (ntime, nonce) = solve_share(&notify, &extranonce1, &extranonce2, difficulty);

    let submit = client
        .submit(notify.job_id, extranonce2.clone(), ntime, nonce)
        .await;

    assert!(submit.is_ok());

    let submit_duplicate = client
        .submit(notify.job_id, extranonce2, ntime, nonce)
        .await;

    assert!(submit_duplicate.is_err());
    assert!(matches!(
        submit_duplicate,
        Err(ClientError::Stratum { response }) if response.error_code == StratumError::Duplicate as i32
    ));
}

#[tokio::test]
async fn clean_jobs_true_on_init_and_new_block() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");
    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    client.subscribe().await.unwrap();
    client.authorize().await.unwrap();

    let mut notify = match events.recv().await.unwrap() {
        stratum::Event::Notify(n) => n,
        stratum::Event::SetDifficulty(_) => match events.recv().await.unwrap() {
            stratum::Event::Notify(n) => n,
            _ => panic!("expected notify"),
        },
        _ => panic!("expected notify"),
    };

    assert!(notify.clean_jobs);

    CommandBuilder::new(format!(
        "miner --mode block-found --username {} {}",
        signet_username(),
        pool.stratum_endpoint()
    ))
    .spawn()
    .wait()
    .unwrap();

    loop {
        match events.recv().await.unwrap() {
            stratum::Event::Notify(notif) if notif.job_id != notify.job_id => {
                notify = notif;
                break;
            }
            _ => {}
        }
    }

    assert!(notify.clean_jobs);
}

#[tokio::test]
async fn shares_must_meet_difficulty() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");
    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();
    let extranonce1 = subscribe.extranonce1;
    let extranonce2 = Extranonce::random(subscribe.extranonce2_size);

    client.authorize().await.unwrap();

    let mut difficulty = stratum::Difficulty::from(1.0);

    let notify = loop {
        match events.recv().await.unwrap() {
            stratum::Event::SetDifficulty(diff) => difficulty = diff,
            stratum::Event::Notify(notify) => break notify,
            _ => {}
        }
    };

    let easy_diff = stratum::Difficulty::from(0.0000001);
    let (ntime, nonce) = solve_share(&notify, &extranonce1, &extranonce2, easy_diff);

    let merkle_root = stratum::merkle_root(
        &notify.coinb1,
        &notify.coinb2,
        &extranonce1,
        &extranonce2,
        &notify.merkle_branches,
    )
    .unwrap();

    let header = Header {
        version: notify.version.into(),
        prev_blockhash: notify.prevhash.clone().into(),
        merkle_root: merkle_root.into(),
        time: ntime.into(),
        bits: notify.nbits.into(),
        nonce: nonce.into(),
    };

    let hash = header.block_hash();
    let pool_target = difficulty.to_target();

    if pool_target.is_met_by(hash) {
        println!("Accidentally found valid share, skipping negative test");
        return;
    }

    let submit = client
        .submit(notify.job_id, extranonce2, ntime, nonce)
        .await;

    assert!(submit.is_err());
    assert!(matches!(
        submit,
        Err(ClientError::Stratum { response }) if response.error_code == StratumError::AboveTarget as i32
    ));
}

#[tokio::test]
async fn stale_share_rejected() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");
    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();
    let extranonce1 = subscribe.extranonce1;
    let extranonce2 = Extranonce::random(subscribe.extranonce2_size);

    client.authorize().await.unwrap();

    let mut difficulty = stratum::Difficulty::from(1.0);

    let notify_a = loop {
        match events.recv().await.unwrap() {
            stratum::Event::SetDifficulty(diff) => difficulty = diff,
            stratum::Event::Notify(notify) => break notify,
            _ => {}
        }
    };

    let (ntime, nonce) = solve_share(&notify_a, &extranonce1, &extranonce2, difficulty);

    CommandBuilder::new(format!(
        "miner --mode block-found --username {} {}",
        signet_username(),
        pool.stratum_endpoint()
    ))
    .spawn()
    .wait()
    .unwrap();

    loop {
        match events.recv().await.unwrap() {
            stratum::Event::Notify(n) if n.job_id != notify_a.job_id && n.clean_jobs => {
                break;
            }
            _ => {}
        }
    }

    let submit = client
        .submit(notify_a.job_id, extranonce2, ntime, nonce)
        .await;

    assert!(submit.is_err());
    assert!(matches!(
        submit,
        Err(ClientError::Stratum { response })
        if response.error_code == StratumError::Stale as i32
        || response.error_code == StratumError::InvalidJobId as i32
    ));
}

#[tokio::test]
async fn invalid_job_id_rejected_as_stale() {
    let pool = TestPool::spawn();
    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();
    let _extranonce1 = subscribe.extranonce1;
    let extranonce2 = Extranonce::random(subscribe.extranonce2_size);

    client.authorize().await.unwrap();

    let _ = events.recv().await.unwrap();
    let _ = events.recv().await.unwrap();

    let ntime = Ntime::from(0);
    let nonce = Nonce::from(0);

    let bad_job_id = stratum::JobId::from(0xdeadbeef);

    let submit = client.submit(bad_job_id, extranonce2, ntime, nonce).await;

    assert!(submit.is_err());
    assert!(matches!(
        submit,
        Err(ClientError::Stratum { response }) if response.error_code == StratumError::Stale as i32
    ));
}
