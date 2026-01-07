use super::*;
use test_proxy::TestProxy;

#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn proxy_connects_to_upstream() {
    let pool = TestPool::spawn();
    let upstream_endpoint = pool.stratum_endpoint();

    let proxy = TestProxy::spawn(&upstream_endpoint, &signet_username().to_string());

    tokio::time::sleep(Duration::from_millis(500)).await;

    let status = proxy
        .get_status()
        .await
        .expect("Failed to get proxy status");

    assert!(
        status.upstream.connected,
        "Proxy should be connected to upstream"
    );

    assert_eq!(
        status.upstream.url, upstream_endpoint,
        "Upstream URL should match"
    );

    assert_eq!(
        status.upstream.username,
        signet_username().to_string(),
        "Username should match"
    );

    assert_eq!(status.downstream.address, "127.0.0.1");
    assert_eq!(
        status.downstream.port,
        proxy
            .stratum_endpoint()
            .split(':')
            .next_back()
            .unwrap()
            .parse::<u16>()
            .unwrap()
    );
}
