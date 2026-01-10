use super::*;

#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn proxy_connects_to_upstream() {
    let pool = TestPool::spawn();
    let upstream = pool.stratum_endpoint();

    let proxy = TestProxy::spawn(&upstream, &signet_username().to_string());

    tokio::time::sleep(Duration::from_millis(500)).await;

    let status = proxy
        .get_status()
        .await
        .expect("Failed to get proxy status");

    assert_eq!(status.upstream, upstream, "Upstream URL should match");

    assert_eq!(
        status.upstream_username,
        signet_username(),
        "Username should match"
    );

    assert!(status.connected, "Proxy should be connected to upstream");
}
