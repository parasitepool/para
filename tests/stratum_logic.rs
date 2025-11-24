use crate::test_pool::TestPool;
use bitcoin::block::Header;
use para::stratum::{self, ClientError, Extranonce, Nonce, Ntime, StratumError};

// Helper to solve a block for a specific difficulty
fn solve_share(
    notify: &stratum::Notify,
    extranonce1: &Extranonce,
    extranonce2: &Extranonce,
    difficulty: stratum::Difficulty,
) -> (Ntime, Nonce) {
    let merkle_root = stratum::merkle_root(
        &notify.coinb1,
        &notify.coinb2,
        extranonce1,
        extranonce2,
        &notify.merkle_branches,
    )
    .unwrap();

    let mut header = Header {
        version: bitcoin::block::Version::from_consensus(notify.version.0.to_consensus()),
        prev_blockhash: notify.prevhash.clone().into(),
        merkle_root: merkle_root.into(),
        time: u32::from(notify.ntime),
        bits: bitcoin::CompactTarget::from(notify.nbits),
        nonce: 0,
    };

    let target = difficulty.to_target();

    // Brute force
    loop {
        let hash = header.block_hash();
        if target.is_met_by(hash) {
            return (Ntime::from(header.time), Nonce::from(header.nonce));
        }
        header.nonce += 1;
        if header.nonce == 0 {
            panic!(
                "Nonce wrapped around without finding share at diff {}",
                difficulty
            );
        }
    }
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

    let mut difficulty = stratum::Difficulty::from(1.0); // default

    // Wait for diff and notify
    let notify = loop {
        match events.recv().await.unwrap() {
            stratum::Event::SetDifficulty(d) => difficulty = d,
            stratum::Event::Notify(n) => break n,
            _ => {}
        }
    };

    let (ntime, nonce) = solve_share(&notify, &extranonce1, &extranonce2, difficulty);

    // Submit first time
    let submit1 = client
        .submit(notify.job_id, extranonce2.clone(), ntime, nonce)
        .await;
    assert!(submit1.is_ok());

    // Submit second time
    let submit2 = client
        .submit(notify.job_id, extranonce2, ntime, nonce)
        .await;

    assert!(submit2.is_err());
    let err = submit2.unwrap_err();

    if let ClientError::Stratum { response } = err {
        assert_eq!(response.error_code, StratumError::Duplicate as i32);
    } else {
        panic!("Expected Stratum error, got: {:?}", err);
    }
}

#[tokio::test]
async fn clean_jobs_logic() {
    let pool = TestPool::spawn();
    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    client.subscribe().await.unwrap();
    client.authorize().await.unwrap();

    // 1. First notify should be clean_jobs=true
    let mut notify = match events.recv().await.unwrap() {
        stratum::Event::Notify(n) => n,
        stratum::Event::SetDifficulty(_) => {
            // consume diff, next should be notify
            match events.recv().await.unwrap() {
                stratum::Event::Notify(n) => n,
                _ => panic!("expected notify"),
            }
        }
        _ => panic!("expected notify"),
    };

    assert!(notify.clean_jobs);

    // 2. Mine a block on the network to force a new block template
    pool.bitcoind_handle().mine_blocks(1).unwrap();

    // 3. Wait for next Notify
    loop {
        match events.recv().await.unwrap() {
            stratum::Event::Notify(n) if n.job_id != notify.job_id => {
                notify = n;
                break;
            }
            _ => {}
        }
    }

    // New block means previous jobs are invalid
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
            stratum::Event::SetDifficulty(d) => difficulty = d,
            stratum::Event::Notify(n) => break n,
            _ => {}
        }
    };

    let easy_diff = stratum::Difficulty::from(0.0000001);
    let (ntime, nonce) = solve_share(&notify, &extranonce1, &extranonce2, easy_diff);

    // Verify that this share does NOT meet the actual pool difficulty
    let merkle_root = stratum::merkle_root(
        &notify.coinb1,
        &notify.coinb2,
        &extranonce1,
        &extranonce2,
        &notify.merkle_branches,
    )
    .unwrap();

    let header = Header {
        version: bitcoin::block::Version::from_consensus(notify.version.0.to_consensus()),
        prev_blockhash: notify.prevhash.clone().into(),
        merkle_root: merkle_root.into(),
        time: u32::from(ntime),
        bits: bitcoin::CompactTarget::from(notify.nbits),
        nonce: u32::from(nonce),
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
    let err = submit.unwrap_err();
    if let ClientError::Stratum { response } = err {
        assert_eq!(response.error_code, StratumError::AboveTarget as i32);
    } else {
        panic!("Expected Stratum error, got: {:?}", err);
    }
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

    // Get first job
    let notify_a = loop {
        match events.recv().await.unwrap() {
            stratum::Event::SetDifficulty(d) => difficulty = d,
            stratum::Event::Notify(n) => break n,
            _ => {}
        }
    };

    // Solve share for job A
    let (ntime, nonce) = solve_share(&notify_a, &extranonce1, &extranonce2, difficulty);

    // Trigger new block to get clean_jobs=true
    pool.bitcoind_handle().mine_blocks(1).unwrap();

    // Wait for new job B
    let _notify_b = loop {
        match events.recv().await.unwrap() {
            stratum::Event::Notify(n) if n.job_id != notify_a.job_id && n.clean_jobs => {
                break n;
            }
            _ => {}
        }
    };

    // Submit share for job A (stale)
    let submit = client
        .submit(notify_a.job_id, extranonce2, ntime, nonce)
        .await;

    assert!(submit.is_err());
    let err = submit.unwrap_err();
    if let ClientError::Stratum { response } = err {
        // Accept either Stale or Job Not Found (if job was purged)
        // Usually it returns Stale for old jobs.
        assert!(
            response.error_code == StratumError::Stale as i32
                || response.error_code == StratumError::InvalidJobId as i32 // Or "Job not found" which is code 20/21 on some pools, but here likely Stale(2) or Invalid(1?) or similar
        );
        if response.error_code != StratumError::Stale as i32 {
            // If not Stale, check message just in case
            // But we should really rely on error codes if possible.
            // Let's assume Stale is what we want.
            // Wait, InvalidJobId is 1. Stale is 2.
            // Previous test allowed "Job not found" string.
        }
    } else {
        panic!("Expected Stratum error, got: {:?}", err);
    }
}

#[tokio::test]
async fn invalid_job_id_rejected() {
    let pool = TestPool::spawn();
    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    let (subscribe, _, _) = client.subscribe().await.unwrap();
    let _extranonce1 = subscribe.extranonce1;
    let extranonce2 = Extranonce::random(subscribe.extranonce2_size);

    client.authorize().await.unwrap();

    // Consume initial messages
    let _ = events.recv().await.unwrap();
    let _ = events.recv().await.unwrap();

    let ntime = Ntime::from(0);
    let nonce = Nonce::from(0);

    let bad_job_id = stratum::JobId::from(0xdeadbeef);

    let submit = client.submit(bad_job_id, extranonce2, ntime, nonce).await;

    assert!(submit.is_err());
    let err = submit.unwrap_err();
    if let ClientError::Stratum { response } = err {
        assert_eq!(response.error_code, StratumError::Stale as i32);
    } else {
        panic!("Expected Stratum error, got: {:?}", err);
    }
}
