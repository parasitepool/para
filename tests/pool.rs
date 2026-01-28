use super::*;

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
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

#[tokio::test]
#[serial(bitcoind)]
#[timeout(120000)]
async fn stratum_state_machine() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001 --disable-bouncer");

    // State::Init
    {
        let client = pool.stratum_client().await;
        client.connect().await.unwrap();

        // configure with unsupported extension -> error
        assert!(
            client
                .configure(vec!["unknown-extension".into()], None)
                .await
                .unwrap_err()
                .to_string()
                .contains("Unsupported extension")
        );

        // authorize before subscribe -> MethodNotAllowed
        assert_stratum_error(client.authorize().await, StratumError::MethodNotAllowed);

        // submit before subscribe -> Unauthorized
        assert_stratum_error(
            client
                .submit(
                    JobId::new(0),
                    Extranonce::random(8),
                    Ntime::from(0),
                    Nonce::from(0),
                    None,
                )
                .await,
            StratumError::Unauthorized,
        );
    }

    // State::Configured
    {
        let client = pool.stratum_client().await;
        client.connect().await.unwrap();

        // configure -> Configured
        client
            .configure(
                vec!["version-rolling".into()],
                Some(Version::from_str("1fffe000").unwrap()),
            )
            .await
            .unwrap();

        // configure again (reconfigure) -> still Configured
        client
            .configure(
                vec!["version-rolling".into()],
                Some(Version::from_str("1fffe000").unwrap()),
            )
            .await
            .unwrap();

        // authorize in Configured -> MethodNotAllowed
        assert_stratum_error(client.authorize().await, StratumError::MethodNotAllowed);

        // submit in Configured -> Unauthorized
        assert_stratum_error(
            client
                .submit(
                    JobId::new(0),
                    Extranonce::random(8),
                    Ntime::from(0),
                    Nonce::from(0),
                    None,
                )
                .await,
            StratumError::Unauthorized,
        );
    }

    // State::Subscribed
    {
        let client = pool.stratum_client().await;
        client.connect().await.unwrap();

        let (subscribe, _, _) = client.subscribe().await.unwrap();
        assert_eq!(subscribe.subscriptions.len(), 2);

        // configure in Subscribed -> MethodNotAllowed
        assert_stratum_error(
            client
                .configure(
                    vec!["version-rolling".into()],
                    Some(Version::from_str("1fffe000").unwrap()),
                )
                .await,
            StratumError::MethodNotAllowed,
        );

        // subscribe again (resubscription in Subscribed) -> MethodNotAllowed
        assert_stratum_error(client.subscribe().await, StratumError::MethodNotAllowed);

        // submit in Subscribed -> Unauthorized
        assert_stratum_error(
            client
                .submit(
                    JobId::new(0),
                    Extranonce::random(8),
                    Ntime::from(0),
                    Nonce::from(0),
                    None,
                )
                .await,
            StratumError::Unauthorized,
        );
    }

    // State::Working
    {
        let client = pool.stratum_client().await;
        let mut events = client.connect().await.unwrap();

        let (subscribe_result, _, _) = client.subscribe().await.unwrap();
        let enonce1 = subscribe_result.enonce1.clone();
        let enonce2_size = subscribe_result.enonce2_size;

        client.authorize().await.unwrap();

        let (notify, difficulty) = wait_for_notify(&mut events).await;

        assert_eq!(difficulty, Difficulty::from(0.00001));
        assert_eq!(notify.job_id, JobId::from(0));
        assert!(notify.clean_jobs);

        // configure in Working -> MethodNotAllowed
        assert_stratum_error(
            client
                .configure(
                    vec!["version-rolling".into()],
                    Some(Version::from_str("1fffe000").unwrap()),
                )
                .await,
            StratumError::MethodNotAllowed,
        );

        // Verify we're still in Working state by submitting a share
        let enonce2_for_state_check = Extranonce::random(enonce2_size);
        let (ntime_check, nonce_check) =
            solve_share(&notify, &enonce1, &enonce2_for_state_check, difficulty);
        client
            .submit(
                notify.job_id,
                enonce2_for_state_check,
                ntime_check,
                nonce_check,
                None,
            )
            .await
            .unwrap();

        // authorize in Working -> MethodNotAllowed
        assert_stratum_error(client.authorize().await, StratumError::MethodNotAllowed);

        // submit in Working -> allowed
        let enonce2 = Extranonce::random(enonce2_size);
        let (ntime, nonce) = solve_share(&notify, &enonce1, &enonce2, difficulty);
        client
            .submit(notify.job_id, enonce2, ntime, nonce, None)
            .await
            .unwrap();
    }

    // Resubscription behavior (same connection)
    {
        let client = pool.stratum_client().await;
        let mut events = client.connect().await.unwrap();

        client
            .configure(
                vec!["version-rolling".into()],
                Some(Version::from_str("1fffe000").unwrap()),
            )
            .await
            .unwrap();

        let (subscribe_result, _, _) = client.subscribe().await.unwrap();
        let enonce1 = subscribe_result.enonce1.clone();
        let enonce2_size = subscribe_result.enonce2_size;

        client.authorize().await.unwrap();

        let (notify, difficulty) = wait_for_notify(&mut events).await;

        // Confirm Working state by submitting valid share
        let enonce2 = Extranonce::random(enonce2_size);
        let (ntime, nonce) = solve_share(&notify, &enonce1, &enonce2, difficulty);
        client
            .submit(notify.job_id, enonce2, ntime, nonce, None)
            .await
            .unwrap();

        // Resubscribe on same connection -> MethodNotAllowed
        assert_stratum_error(client.subscribe().await, StratumError::MethodNotAllowed);
    }

    // Successful session resume preserves enonce1
    let original_enonce1 = {
        let client = pool.stratum_client().await;
        let mut events = client.connect().await.unwrap();

        let (subscribe_result, _, _) = client.subscribe().await.unwrap();
        let enonce1 = subscribe_result.enonce1.clone();

        client.authorize().await.unwrap();

        // Must submit a share for session to be stored (requires authorization)
        let (notify, difficulty) = wait_for_notify(&mut events).await;
        let enonce2 = Extranonce::random(subscribe_result.enonce2_size);
        let (ntime, nonce) = solve_share(&notify, &enonce1, &enonce2, difficulty);
        client
            .submit(notify.job_id, enonce2, ntime, nonce, None)
            .await
            .unwrap();

        client.disconnect().await;
        enonce1
    };

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Resume session with original enonce1
    {
        let client = pool.stratum_client().await;
        let mut events = client.connect().await.unwrap();

        let (subscribe_result, _, _) = client
            .subscribe_with_enonce1(Some(original_enonce1.clone()))
            .await
            .unwrap();

        assert_eq!(
            subscribe_result.enonce1, original_enonce1,
            "Session resumption should return the same enonce1"
        );

        // Must re-authorize even after session resume
        client.authorize().await.unwrap();

        // Should be able to work with resumed session
        let (notify, difficulty) = wait_for_notify(&mut events).await;
        let enonce2 = Extranonce::random(subscribe_result.enonce2_size);
        let (ntime, nonce) = solve_share(&notify, &original_enonce1, &enonce2, difficulty);
        client
            .submit(notify.job_id, enonce2, ntime, nonce, None)
            .await
            .unwrap();
    }

    // Unknown enonce1 results in new enonce1
    {
        let client = pool.stratum_client().await;
        client.connect().await.unwrap();

        let fake_enonce1: Extranonce = "deadbeef".parse().unwrap();
        let (subscribe_result, _, _) = client
            .subscribe_with_enonce1(Some(fake_enonce1.clone()))
            .await
            .unwrap();

        assert_ne!(
            subscribe_result.enonce1, fake_enonce1,
            "Unknown enonce1 should result in new enonce1 being issued"
        );

        client.authorize().await.unwrap();
    }

    // Authorization Validation
    {
        let client_invalid_username = pool.stratum_client_for_username("notabitcoinaddress").await;
        let client_address_wrong_network = pool
            .stratum_client_for_username("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4")
            .await;

        client_invalid_username.connect().await.unwrap();
        client_address_wrong_network.connect().await.unwrap();

        client_invalid_username.subscribe().await.unwrap();
        client_address_wrong_network.subscribe().await.unwrap();

        assert!(
            client_invalid_username
                .authorize()
                .await
                .unwrap_err()
                .to_string()
                .contains("Invalid bitcoin address")
        );

        assert!(
            client_address_wrong_network
                .authorize()
                .await
                .unwrap_err()
                .to_string()
                .contains(
                    "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4 is not valid for signet network"
                )
        );
    }
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
#[ignore]
async fn clean_jobs_true_on_init_and_new_block() {
    let pool = TestPool::spawn_with_args("--start-diff 0.0001");
    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    client.subscribe().await.unwrap();
    client.authorize().await.unwrap();

    let (notify, _) = wait_for_notify(&mut events).await;
    assert!(notify.clean_jobs);

    pool.mine_block();

    let notify = wait_for_new_block(&mut events, notify.job_id).await;
    assert!(notify.clean_jobs);
}

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
fn configure_template_update_interval() {
    let pool = TestPool::spawn_with_args("--update-interval 1 --start-diff 0.00001");

    let stratum_endpoint = pool.stratum_endpoint();

    let output = CommandBuilder::new(format!(
        "template {stratum_endpoint} --username {} --raw",
        signet_username()
    ))
    .spawn()
    .wait_with_output()
    .unwrap();

    let t1 =
        serde_json::from_str::<stratum::Notify>(&String::from_utf8_lossy(&output.stdout)).unwrap();

    std::thread::sleep(Duration::from_secs(1));

    let output = CommandBuilder::new(format!(
        "template {stratum_endpoint} --username {} --raw",
        signet_username()
    ))
    .spawn()
    .wait_with_output()
    .unwrap();

    let t2 =
        serde_json::from_str::<stratum::Notify>(&String::from_utf8_lossy(&output.stdout)).unwrap();

    assert!(t1.ntime < t2.ntime);
}

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
#[ignore]
fn concurrently_listening_workers_receive_new_templates_on_new_block() {
    let pool = TestPool::spawn_with_args("--start-diff 0.0001");
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
                let mut template_watcher = CommandBuilder::new(format!(
                    "template {endpoint} --username {user} --watch --raw"
                ))
                .spawn();

                let mut reader = BufReader::new(template_watcher.stdout.take().unwrap());

                let initial_template = next_json::<stratum::Notify>(&mut reader);

                gate.wait();

                let new_template = next_json::<stratum::Notify>(&mut reader);

                out.send((initial_template, new_template)).ok();

                template_watcher.kill().unwrap();
                template_watcher.wait().unwrap();
            });
        }

        gate.wait();

        pool.mine_block();

        let (initial_template_worker_a, new_template_worker_a) =
            in_1.recv_timeout(Duration::from_secs(10)).unwrap();

        let (initial_template_worker_b, new_template_worker_b) =
            in_2.recv_timeout(Duration::from_secs(10)).unwrap();

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
#[serial(bitcoind)]
#[timeout(120000)]
async fn vardiff_adjusts_difficulty() {
    let pool = TestPool::spawn_with_args(
        "--start-diff 0.00001 --vardiff-period 1 --vardiff-window 5 --disable-bouncer",
    );

    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();
    let enonce1 = subscribe.enonce1;

    client.authorize().await.unwrap();

    let (notify, initial_difficulty) = wait_for_notify(&mut events).await;

    assert_eq!(
        initial_difficulty,
        Difficulty::from(0.00001),
        "Start difficulty should match configured value"
    );

    let mut accepted_shares = 0;
    for _ in 0..30 {
        let enonce2 = Extranonce::random(subscribe.enonce2_size);
        let (ntime, nonce) = solve_share(&notify, &enonce1, &enonce2, initial_difficulty);

        match client
            .submit(notify.job_id, enonce2, ntime, nonce, None)
            .await
        {
            Ok(_) => accepted_shares += 1,
            Err(ClientError::Stratum { response })
                if response.error_code == StratumError::Duplicate as i32 =>
            {
                continue;
            }
            Err(ClientError::Stratum { response })
                if response.error_code == StratumError::AboveTarget as i32 =>
            {
                continue;
            }
            Err(ClientError::Stratum { response })
                if response.error_code == StratumError::Stale as i32 =>
            {
                continue;
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        }

        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(
        accepted_shares >= 3,
        "Need at least 3 accepted shares, got {}",
        accepted_shares
    );

    let new_difficulty = timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await {
                Ok(stratum::Event::SetDifficulty(diff)) => return diff,
                Ok(_) => continue,
                Err(e) => panic!("Event channel closed unexpectedly: {:?}", e),
            }
        }
    })
    .await
    .expect("Timeout waiting for difficulty adjustment");

    assert!(
        new_difficulty > initial_difficulty,
        "Difficulty should increase when shares come faster than target: {} -> {}",
        initial_difficulty,
        new_difficulty
    );
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn share_validation() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001 --disable-bouncer");

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.endpoint, pool.stratum_endpoint());
    assert_eq!(status.users, 0);
    assert_eq!(status.workers, 0);
    assert_eq!(status.connections, 0);
    assert_eq!(status.blocks, 0);
    assert_eq!(status.accepted, 0);
    assert_eq!(status.rejected, 0);
    assert!(status.best_ever.is_none());
    assert!(status.last_share.is_none());

    let system_status = pool.get_system_status().await.unwrap();
    assert!(system_status.cpu_usage_percent >= 0.0 && system_status.cpu_usage_percent <= 100.0);
    assert!(
        system_status.memory_usage_percent >= 0.0 && system_status.memory_usage_percent <= 100.0
    );
    assert!(system_status.disk_usage_percent >= 0.0 && system_status.disk_usage_percent <= 100.0);
    assert!(system_status.uptime > 0);

    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();
    let enonce1 = subscribe.enonce1;
    let enonce2_size = subscribe.enonce2_size;

    client.authorize().await.unwrap();

    let (notify, difficulty) = wait_for_notify(&mut events).await;
    let username = signet_username();
    let user_address = username
        .parse_address()
        .unwrap()
        .assume_checked()
        .to_string();

    // Valid share accepted
    let enonce2 = Extranonce::random(enonce2_size);
    let (ntime, nonce) = solve_share(&notify, &enonce1, &enonce2, difficulty);
    client
        .submit(notify.job_id, enonce2.clone(), ntime, nonce, None)
        .await
        .unwrap();

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.users, 1);
    assert_eq!(status.workers, 1);
    assert_eq!(status.connections, 1);
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 0);
    assert!(status.best_ever.is_some());
    assert!(status.last_share.is_some());

    let user = pool.get_user(&user_address).await.unwrap();
    assert_eq!(user.address, user_address);
    assert_eq!(user.accepted, 1);
    assert_eq!(user.rejected, 0);
    assert!(user.best_ever.is_some());
    assert_eq!(user.workers.len(), 1);
    assert_eq!(user.workers[0].accepted, 1);
    assert_eq!(user.workers[0].rejected, 0);
    assert!(user.workers[0].best_ever.is_some());

    // Duplicate rejected
    assert_stratum_error(
        client
            .submit(notify.job_id, enonce2, ntime, nonce, None)
            .await,
        StratumError::Duplicate,
    );

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 1);

    let user = pool.get_user(&user_address).await.unwrap();
    assert_eq!(user.accepted, 1);
    assert_eq!(user.rejected, 1);
    assert_eq!(user.workers[0].accepted, 1);
    assert_eq!(user.workers[0].rejected, 1);

    // Invalid enonce2 length (too short)
    assert_stratum_error(
        client
            .submit(
                notify.job_id,
                Extranonce::random(enonce2_size - 1),
                ntime,
                nonce,
                None,
            )
            .await,
        StratumError::InvalidNonce2Length,
    );

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 2);

    // Invalid enonce2 length (too long)
    assert_stratum_error(
        client
            .submit(
                notify.job_id,
                Extranonce::random(enonce2_size + 1),
                ntime,
                nonce,
                None,
            )
            .await,
        StratumError::InvalidNonce2Length,
    );

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 3);

    // Invalid job id (stale)
    assert_stratum_error(
        client
            .submit(
                JobId::from(0xdeadbeef),
                Extranonce::random(enonce2_size),
                ntime,
                nonce,
                None,
            )
            .await,
        StratumError::Stale,
    );

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 4);

    // Share above target
    assert_stratum_error(
        client
            .submit(
                notify.job_id,
                Extranonce::random(enonce2_size),
                notify.ntime,
                Nonce::from(0),
                None,
            )
            .await,
        StratumError::AboveTarget,
    );

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 5);

    // Worker mismatch rejected
    assert_stratum_error(
        client
            .submit_with_username(
                Username::from("different_address.different_worker"),
                notify.job_id,
                Extranonce::random(enonce2_size),
                notify.ntime,
                Nonce::from(0),
                None,
            )
            .await,
        StratumError::WorkerMismatch,
    );

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 6);

    let user = pool.get_user(&user_address).await.unwrap();
    assert_eq!(user.accepted, 1);
    assert_eq!(user.rejected, 6);

    // Ntime before job's ntime rejected
    let job_ntime: u32 = notify.ntime.into();
    assert_stratum_error(
        client
            .submit(
                notify.job_id,
                Extranonce::random(enonce2_size),
                Ntime::from(job_ntime - 1),
                Nonce::from(0),
                None,
            )
            .await,
        StratumError::NtimeOutOfRange,
    );

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 7);

    // Ntime too far in future rejected (> 7000 seconds)
    assert_stratum_error(
        client
            .submit(
                notify.job_id,
                Extranonce::random(enonce2_size),
                Ntime::from(job_ntime + 7001),
                Nonce::from(0),
                None,
            )
            .await,
        StratumError::NtimeOutOfRange,
    );

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 8);

    let user = pool.get_user(&user_address).await.unwrap();
    assert_eq!(user.accepted, 1);
    assert_eq!(user.rejected, 8);

    // Version bits submitted without negotiation -> InvalidVersionMask
    let enonce2_vr = Extranonce::random(enonce2_size);
    let (ntime_vr, nonce_vr) = solve_share(&notify, &enonce1, &enonce2_vr, difficulty);
    assert_stratum_error(
        client
            .submit(
                notify.job_id,
                enonce2_vr,
                ntime_vr,
                nonce_vr,
                Some(Version::from_str("00100000").unwrap()),
            )
            .await,
        StratumError::InvalidVersionMask,
    );

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.accepted, 1);
    assert_eq!(status.rejected, 9);

    // Stale after new block
    let old_job_id = notify.job_id;
    let fresh_enonce2 = Extranonce::random(enonce2_size);
    let (old_ntime, old_nonce) = solve_share(&notify, &enonce1, &fresh_enonce2, difficulty);

    pool.mine_block();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let baseline = pool.get_status().await.unwrap();
    let user_baseline = pool.get_user(&user_address).await.unwrap();

    assert_stratum_error(
        client
            .submit(old_job_id, fresh_enonce2, old_ntime, old_nonce, None)
            .await,
        StratumError::Stale,
    );

    let status = pool.get_status().await.unwrap();
    assert_eq!(status.rejected, baseline.rejected + 1);
    assert_eq!(status.blocks, 1);

    let user = pool.get_user(&user_address).await.unwrap();
    assert_eq!(user.rejected, user_baseline.rejected + 1);
    assert_eq!(
        user.workers[0].rejected,
        user_baseline.workers[0].rejected + 1
    );

    // Version rolling validation (new connection with configure)
    {
        let client = pool.stratum_client().await;
        let mut events = client.connect().await.unwrap();

        let (configure_response, _, _) = client
            .configure(
                vec!["version-rolling".into()],
                Some(Version::from_str("1fffe000").unwrap()),
            )
            .await
            .unwrap();

        assert!(configure_response.version_rolling);
        assert_eq!(
            configure_response.version_rolling_mask,
            Some(Version::from_str("1fffe000").unwrap())
        );

        let (subscribe, _, _) = client.subscribe().await.unwrap();
        let enonce1 = subscribe.enonce1;
        let enonce2_size = subscribe.enonce2_size;

        client.authorize().await.unwrap();

        let (notify, difficulty) = wait_for_notify(&mut events).await;
        let version_mask = Version::from_str("1fffe000").unwrap();

        // Valid version_bits within mask -> accepted
        let version_bits_valid = Version::from_str("00100000").unwrap();
        let enonce2_valid = Extranonce::random(enonce2_size);
        let (ntime_valid, nonce_valid) = solve_share_with_version_bits(
            &notify,
            &enonce1,
            &enonce2_valid,
            difficulty,
            Some(version_bits_valid),
            Some(version_mask),
        );
        client
            .submit(
                notify.job_id,
                enonce2_valid,
                ntime_valid,
                nonce_valid,
                Some(version_bits_valid),
            )
            .await
            .unwrap();

        // Zero version_bits -> accepted (treated as no modification)
        let enonce2_zero = Extranonce::random(enonce2_size);
        let (ntime_zero, nonce_zero) = solve_share(&notify, &enonce1, &enonce2_zero, difficulty);
        client
            .submit(
                notify.job_id,
                enonce2_zero,
                ntime_zero,
                nonce_zero,
                Some(Version::from(0)),
            )
            .await
            .unwrap();

        // Disallowed version_bits (outside mask) -> InvalidVersionMask
        assert_stratum_error(
            client
                .submit(
                    notify.job_id,
                    Extranonce::random(enonce2_size),
                    notify.ntime,
                    Nonce::from(0),
                    Some(Version::from_str("e0000000").unwrap()),
                )
                .await,
            StratumError::InvalidVersionMask,
        );
    }
}

#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn bouncer() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");

    let auth_timeout_test = async {
        let client = pool.stratum_client().await;
        let _events = client.connect().await.unwrap();

        client.subscribe().await.unwrap();

        tokio::time::sleep(Duration::from_secs(4)).await;

        let result = client.authorize().await;
        assert!(
            result.is_err(),
            "auth_timeout: Expected connection to be dropped after AUTH_TIMEOUT"
        );
    };

    let idle_timeout_test = async {
        let client = pool.stratum_client().await;
        let mut events = client.connect().await.unwrap();

        let (subscribe, _, _) = client.subscribe().await.unwrap();
        let enonce1 = subscribe.enonce1;
        let enonce2_size = subscribe.enonce2_size;

        client.authorize().await.unwrap();

        let (notify, difficulty) = wait_for_notify(&mut events).await;

        let enonce2 = Extranonce::random(enonce2_size);
        let (ntime, nonce) = solve_share(&notify, &enonce1, &enonce2, difficulty);
        client
            .submit(notify.job_id, enonce2, ntime, nonce, None)
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_secs(8)).await;

        let result = client
            .submit(
                notify.job_id,
                Extranonce::random(enonce2_size),
                ntime,
                nonce,
                None,
            )
            .await;

        assert!(
            result.is_err(),
            "idle_timeout: Expected connection to be dropped after IDLE_TIMEOUT"
        );
    };

    let reject_escalation_test = async {
        let client = pool.stratum_client().await;
        let mut events = client.connect().await.unwrap();

        let (subscribe, _, _) = client.subscribe().await.unwrap();
        let enonce2_size = subscribe.enonce2_size;

        client.authorize().await.unwrap();

        let (initial_notify, _) = wait_for_notify(&mut events).await;
        let initial_job_id = initial_notify.job_id;

        let start = std::time::Instant::now();
        let mut fresh_job_received = false;
        let mut last_job_id = initial_job_id;

        loop {
            let elapsed = start.elapsed();

            let result = client
                .submit(
                    initial_notify.job_id,
                    Extranonce::random(enonce2_size),
                    initial_notify.ntime,
                    Nonce::from(0),
                    None,
                )
                .await;

            match &result {
                Err(ClientError::Io { .. })
                | Err(ClientError::NotConnected)
                | Err(ClientError::ChannelRecv { .. }) => break,
                _ => {}
            }

            while let Some(Ok(event)) = events.try_recv() {
                if let stratum::Event::Notify(notify) = event
                    && notify.job_id != last_job_id
                {
                    last_job_id = notify.job_id;
                    fresh_job_received = true;
                }
            }

            if elapsed > Duration::from_secs(10) {
                panic!(
                    "reject_escalation: Connection still alive after 10s - expected drop at DROP_THRESHOLD (3s)"
                );
            }
        }

        assert!(
            fresh_job_received,
            "reject_escalation: Expected fresh job notification at WARN_THRESHOLD"
        );
    };

    let auth_failure_test = async {
        let client = pool.stratum_client_for_username("invalid.user").await;
        client.connect().await.unwrap();
        client.subscribe().await.unwrap();

        let start = std::time::Instant::now();
        let mut dropped = false;

        while start.elapsed() < Duration::from_secs(10) {
            match client.authorize().await {
                Ok(_) => panic!("auth_failure: Expected unauthorized response"),
                Err(ClientError::NotConnected) | Err(ClientError::Io { .. }) => {
                    dropped = true;
                    break;
                }
                Err(err) => {
                    assert!(
                        matches!(
                            err,
                            ClientError::Stratum { ref response }
                                if response.error_code == StratumError::Unauthorized as i32
                        ),
                        "auth_failure: Expected Unauthorized, got {err:?}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }

        assert!(
            dropped,
            "auth_failure: Expected connection to be dropped after auth failures and bouncer escalation"
        );
    };

    let authorize_before_subscribe_test = async {
        let client = pool.stratum_client().await;
        client.connect().await.unwrap();

        let start = std::time::Instant::now();
        let mut dropped = false;

        while start.elapsed() < Duration::from_secs(10) {
            match client.authorize().await {
                Ok(_) => panic!("auth_before_subscribe: Expected MethodNotAllowed response"),
                Err(ClientError::NotConnected) | Err(ClientError::Io { .. }) => {
                    dropped = true;
                    break;
                }
                Err(err) => {
                    assert!(
                        matches!(
                            err,
                            ClientError::Stratum { ref response }
                                if response.error_code == StratumError::MethodNotAllowed as i32
                        ),
                        "auth_before_subscribe: Expected MethodNotAllowed, got {err:?}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }

        assert!(
            dropped,
            "auth_before_subscribe: Expected connection to be dropped after repeated authorize-before-subscribe attempts"
        );
    };

    let submit_before_authorize_test = async {
        let client = pool.stratum_client().await;
        client.connect().await.unwrap();
        client.subscribe().await.unwrap();

        let start = std::time::Instant::now();
        let mut dropped = false;

        while start.elapsed() < Duration::from_secs(10) {
            match client
                .submit(
                    JobId::new(0),
                    Extranonce::random(8),
                    Ntime::from(0),
                    Nonce::from(0),
                    None,
                )
                .await
            {
                Ok(_) => panic!("submit_before_authorize: Expected unauthorized response"),
                Err(ClientError::NotConnected) | Err(ClientError::Io { .. }) => {
                    dropped = true;
                    break;
                }
                Err(err) => {
                    assert!(
                        matches!(
                            err,
                            ClientError::Stratum { ref response }
                                if response.error_code == StratumError::Unauthorized as i32
                        ),
                        "submit_before_authorize: Expected Unauthorized, got {err:?}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }

        assert!(
            dropped,
            "submit_before_authorize: Expected connection to be dropped after repeated submit-before-authorize attempts"
        );
    };

    let duplicate_subscribe_test = async {
        let client = pool.stratum_client().await;
        client.connect().await.unwrap();
        client.subscribe().await.unwrap();

        let start = std::time::Instant::now();
        let mut dropped = false;

        while start.elapsed() < Duration::from_secs(10) {
            match client.subscribe().await {
                Ok(_) => panic!("duplicate_subscribe: Expected MethodNotAllowed response"),
                Err(ClientError::NotConnected) | Err(ClientError::Io { .. }) => {
                    dropped = true;
                    break;
                }
                Err(err) => {
                    assert!(
                        matches!(
                            err,
                            ClientError::Stratum { ref response }
                                if response.error_code == StratumError::MethodNotAllowed as i32
                        ),
                        "duplicate_subscribe: Expected MethodNotAllowed, got {err:?}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }

        assert!(
            dropped,
            "duplicate_subscribe: Expected connection to be dropped after repeated subscribe attempts"
        );
    };

    let duplicate_authorize_test = async {
        let client = pool.stratum_client().await;
        client.connect().await.unwrap();
        client.subscribe().await.unwrap();
        client.authorize().await.unwrap();

        let start = std::time::Instant::now();
        let mut dropped = false;

        while start.elapsed() < Duration::from_secs(10) {
            match client.authorize().await {
                Ok(_) => panic!("duplicate_authorize: Expected MethodNotAllowed response"),
                Err(ClientError::NotConnected) | Err(ClientError::Io { .. }) => {
                    dropped = true;
                    break;
                }
                Err(err) => {
                    assert!(
                        matches!(
                            err,
                            ClientError::Stratum { ref response }
                                if response.error_code == StratumError::MethodNotAllowed as i32
                        ),
                        "duplicate_authorize: Expected MethodNotAllowed, got {err:?}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }

        assert!(
            dropped,
            "duplicate_authorize: Expected connection to be dropped after repeated authorize attempts"
        );
    };

    tokio::join!(
        auth_timeout_test,
        idle_timeout_test,
        reject_escalation_test,
        auth_failure_test,
        authorize_before_subscribe_test,
        submit_before_authorize_test,
        duplicate_subscribe_test,
        duplicate_authorize_test
    );
}
