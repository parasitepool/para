use super::*;
use serde::Deserialize;

/// Pool stats from the API (duplicated here since api module is private)
#[derive(Debug, Clone, Deserialize)]
pub struct PoolStats {
    pub connections: u64,
    pub sessions: usize,
    pub active_sessions: usize,
    // Other fields exist but we don't need them for tests
}

/// Session summary from the API (duplicated here since api module is private)
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct SessionSummary {
    pub enonce1: String,
    pub address: String,
    pub workername: String,
    pub created_at_secs: u64,
    pub ttl_remaining_secs: u64,
}

/// Test that the API sessions endpoint works
#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn api_sessions_endpoint_returns_empty_initially() {
    let pool = TestPool::spawn();

    let response = reqwest::get(format!("{}/api/sessions", pool.api_endpoint()))
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let sessions: Vec<SessionSummary> = response.json().await.unwrap();
    assert!(sessions.is_empty());
}

/// Test that the API stats endpoint includes session counts
#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn api_stats_includes_session_counts() {
    let pool = TestPool::spawn();

    let response = reqwest::get(format!("{}/api/stats", pool.api_endpoint()))
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let stats: PoolStats = response.json().await.unwrap();
    assert_eq!(stats.sessions, 0);
    assert_eq!(stats.active_sessions, 0);
}

/// Test that session is stored after miner disconnects
#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn session_stored_after_disconnect() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001 --session-ttl 30");

    // Connect and authorize a client
    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    // Subscribe
    let (subscribe, _, _) = client.subscribe().await.unwrap();
    assert!(!subscribe.enonce1.to_string().is_empty());

    // Authorize
    assert!(client.authorize().await.is_ok());

    // Wait for set_difficulty and notify
    let _ = events.recv().await;
    let _ = events.recv().await;

    // Explicitly disconnect to ensure the TCP connection closes
    let _ = client.disconnect().await;

    // Give pool time to process the disconnect
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check that a session was stored
    let response = reqwest::get(format!("{}/api/sessions", pool.api_endpoint()))
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let sessions: Vec<SessionSummary> = response.json().await.unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "Expected 1 stored session after disconnect"
    );
}

/// Test that pool stats update with connections
#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn api_stats_shows_connections() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");

    // Check initial state
    let stats: PoolStats = reqwest::get(format!("{}/api/stats", pool.api_endpoint()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats.connections, 0);

    // Connect a client
    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    // Subscribe and authorize
    client.subscribe().await.unwrap();
    client.authorize().await.unwrap();

    // Wait for notifications
    let _ = events.recv().await;
    let _ = events.recv().await;

    // Give pool time to update stats
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Check connection count
    let stats: PoolStats = reqwest::get(format!("{}/api/stats", pool.api_endpoint()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats.connections, 1);
}

/// Test that new CLI arguments are recognized
#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn cli_reject_thresholds_accepted() {
    // Test that the pool starts successfully with custom reject thresholds
    let pool = TestPool::spawn_with_args(
        "--start-diff 0.00001 \
         --reject-warn-threshold 30 \
         --reject-reconnect-threshold 60 \
         --reject-drop-threshold 90 \
         --session-ttl 120",
    );

    // If we get here, the pool started successfully with the new args
    let stats: PoolStats = reqwest::get(format!("{}/api/stats", pool.api_endpoint()))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Basic sanity check
    assert_eq!(stats.connections, 0);
}

/// Test basic stratum protocol still works after rename
#[tokio::test]
#[serial(bitcoind)]
#[timeout(90000)]
async fn basic_stratum_handshake_works() {
    let pool = TestPool::spawn_with_args("--start-diff 0.00001");

    let client = pool.stratum_client().await;
    let mut events = client.connect().await.unwrap();

    // Subscribe
    let (subscribe, _, _) = client.subscribe().await.unwrap();
    assert_eq!(subscribe.subscriptions.len(), 2);
    assert!(!subscribe.enonce1.to_string().is_empty());

    // Authorize
    assert!(client.authorize().await.is_ok());

    // Should receive set_difficulty notification
    let event = events.recv().await.unwrap();
    assert!(matches!(event, para::stratum::Event::SetDifficulty(_)));

    // Should receive mining.notify notification
    let event = events.recv().await.unwrap();
    assert!(matches!(event, para::stratum::Event::Notify(_)));
}
