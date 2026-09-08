use super::*;

fn node_token(database_url: &str, args: &str) -> Result<String, String> {
    let output = CommandBuilder::new(format!("node-token --database-url {database_url} {args}"))
        .command()
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    if output.status.success() {
        Ok(stdout)
    } else {
        Err(stderr)
    }
}

fn mint(database_url: &str, name: &str) -> String {
    let token = node_token(database_url, &format!("mint --name {name}")).unwrap();
    assert_eq!(token.lines().count(), 1);
    assert_eq!(token.trim().len(), 43);
    token.trim().to_string()
}

fn batch(hostname: &str) -> ShareBatch {
    ShareBatch {
        block: None,
        shares: create_test_shares(3, 800000),
        hostname: hostname.to_string(),
        batch_id: BATCH_COUNTER.fetch_add(1, Ordering::SeqCst) as u64,
        total_shares: 3,
        start_id: 1,
        end_id: 3,
    }
}

#[tokio::test]
async fn sync_with_node_token() {
    let mut server = TestServer::spawn_with_db_args("--admin-token verysecrettoken").await;

    let db_url = server.database_url().unwrap();

    setup_test_schema(db_url.clone()).await.unwrap();

    let token = mint(&db_url, "test-node-1");
    let other = mint(&db_url, "test-node-2");
    assert_ne!(token, other);

    let response = server
        .post_json_raw("/sync/batch", &batch("test-node-1"))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    server.admin_token = Some(other.clone());
    let response = server
        .post_json_raw("/sync/batch", &batch("test-node-1"))
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    server.admin_token = Some(token.clone());
    let response: SyncResponse = server.post_json("/sync/batch", &batch("test-node-1")).await;
    assert_eq!(response.status, "OK");
    assert_eq!(response.received_count, 3);

    let list = node_token(&db_url, "list").unwrap();
    let node_1 = list.lines().find(|l| l.starts_with("test-node-1")).unwrap();
    let node_2 = list.lines().find(|l| l.starts_with("test-node-2")).unwrap();
    assert!(node_1.contains("revoked -"));
    assert!(!node_1.contains("last seen -"));
    assert!(node_2.contains("last seen -"));

    node_token(&db_url, "revoke --name test-node-1").unwrap();
    let response = server
        .post_json_raw("/sync/batch", &batch("test-node-1"))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let err = node_token(&db_url, "revoke --name test-node-1").unwrap_err();
    assert!(err.contains("no active token for test-node-1"), "{err}");

    let rotated = mint(&db_url, "test-node-1");
    assert_ne!(rotated, token);

    let response = server
        .post_json_raw("/sync/batch", &batch("test-node-1"))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    server.admin_token = Some(rotated);
    let response: SyncResponse = server.post_json("/sync/batch", &batch("test-node-1")).await;
    assert_eq!(response.status, "OK");

    server.admin_token = Some("verysecrettoken".into());
    let response: SyncResponse = server.post_json("/sync/batch", &batch("anything")).await;
    assert_eq!(response.status, "OK");
}
