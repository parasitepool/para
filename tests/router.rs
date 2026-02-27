use super::*;

#[tokio::test]
#[serial(bitcoind)]
#[timeout(120000)]
async fn router_round_robin() {
    let pool_a = TestPool::spawn_with_args("--start-diff 0.00001");
    let pool_b = TestPool::spawn_with_args("--start-diff 0.00001");

    let username_a = "tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.foo";
    let username_b = "tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.bar";

    let mut router = TestRouter::spawn(
        &[
            (username_a, &pool_a.stratum_endpoint()),
            (username_b, &pool_b.stratum_endpoint()),
        ],
        pool_a.bitcoind_rpc_port(),
        "--start-diff 0.00001",
    );

    let status = router.get_status().await.unwrap();
    assert_eq!(status.slots.len(), 2);
    assert_eq!(status.session_count, 0);

    let mut miners = Vec::new();

    for _ in 0..3 {
        miners.push(
            CommandBuilder::new(format!(
                "miner {} --mode continuous --username {} --cpu-cores 1",
                router.stratum_endpoint(),
                signet_username()
            ))
            .spawn(),
        );
    }

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.session_count >= 3
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for 3 sessions");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.slots[0].session_count, 2);
    assert_eq!(status.slots[1].session_count, 1);

    drop(pool_a);

    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(status) = router.get_status().await
                && status.slots.len() == 1
                && status.session_count >= 3
            {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for miners to reconnect to remaining upstream");

    let status = router.get_status().await.unwrap();
    assert_eq!(status.slots.len(), 1);
    assert_eq!(status.session_count, 3);

    drop(pool_b);

    timeout(Duration::from_secs(30), async {
        loop {
            if router.try_wait().unwrap().is_some() {
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("Timeout waiting for router to exit");

    for mut miner in miners {
        miner.kill().unwrap();
        miner.wait().unwrap();
    }
}
