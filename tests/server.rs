use super::*;

#[test]
fn pool_status_zero() {
    let server = TestServer::spawn();

    fs::write(
        server.log_dir().join("pool/pool.status"),
        zero_status().to_string(),
    )
    .unwrap();

    server.assert_response("/pool/pool.status", &zero_status().to_string());
}

#[test]
fn pool_status_typical() {
    let server = TestServer::spawn();

    fs::write(
        server.log_dir().join("pool/pool.status"),
        typical_status().to_string(),
    )
    .unwrap();

    server.assert_response("/pool/pool.status", &typical_status().to_string());
}

#[test]
fn user_status_zero() {
    let server = TestServer::spawn();
    let user = zero_user();
    let user_address = address(0);

    let user_str = serde_json::to_string(&user).unwrap();

    fs::write(
        server.log_dir().join(format!("users/{user_address}")),
        &user_str,
    )
    .unwrap();

    server.assert_response(format!("/users/{user_address}"), &user_str);
}

#[test]
fn user_status_typical() {
    let server = TestServer::spawn();
    let user = typical_user();
    let user_address = address(0);

    let user_str = serde_json::to_string(&user).unwrap();

    fs::write(
        server.log_dir().join(format!("users/{user_address}")),
        &user_str,
    )
    .unwrap();

    server.assert_response(format!("/users/{user_address}"), &user_str);
}

#[test]
fn list_users() {
    let server = TestServer::spawn();
    let mut users = BTreeMap::new();
    for i in 0..9 {
        let user = typical_user();
        let user_address = address(i);
        let user_str = serde_json::to_string(&user).unwrap();

        users.insert(user_address.to_string(), user);

        fs::write(
            server.log_dir().join(format!("users/{user_address}")),
            &user_str,
        )
        .unwrap();
    }

    let users_response = server.get_json::<Vec<String>>("/users");

    assert_eq!(users_response.len(), users.len());
    assert_eq!(
        users_response.into_iter().collect::<HashSet<String>>(),
        users.into_keys().collect::<HashSet<String>>()
    );
}

#[test]
fn aggregate_pool_status() {
    let mut servers = Vec::new();
    for _ in 0..3 {
        let server = TestServer::spawn();
        fs::write(
            server.log_dir().join("pool/pool.status"),
            typical_status().to_string(),
        )
        .unwrap();

        servers.push(server)
    }

    assert_eq!(servers.len(), 3);

    let aggregator = TestServer::spawn_with_args(format!(
        "--nodes {} --nodes {} --nodes {}",
        servers[0].url(),
        servers[1].url(),
        servers[2].url()
    ));

    aggregator.assert_response(
        "/aggregator/pool/pool.status",
        &(typical_status() + typical_status() + typical_status()).to_string(),
    );
}

#[test]
fn aggregate_users() {
    let mut users = Vec::new();
    for i in 0..9 {
        let user = typical_user();
        let user_address = address(i);

        users.push((user_address.to_string(), user));
    }

    assert_eq!(users.len(), 9);

    let mut servers = Vec::new();
    for (address, user) in users.iter().take(3) {
        let server = TestServer::spawn();

        fs::write(
            server.log_dir().join(format!("users/{address}")),
            serde_json::to_string(&user).unwrap(),
        )
        .unwrap();

        servers.push(server)
    }

    assert_eq!(servers.len(), 3);

    let aggregator = TestServer::spawn_with_args(format!(
        "--nodes {} --nodes {} --nodes {}",
        servers[0].url(),
        servers[1].url(),
        servers[2].url()
    ));

    for (address, user) in users.iter().take(3) {
        let response = aggregator.get_json::<User>(format!("/aggregator/users/{address}"));
        pretty_assert_eq!(response, *user);
    }
}

#[test]
fn healthcheck_json() {
    let server = TestServer::spawn();

    let healthcheck = server.get_json::<Healthcheck>("/healthcheck");

    assert!(healthcheck.disk_usage_percent > 0.0);
}

#[test]
fn healthcheck_with_auth() {
    let server = TestServer::spawn_with_args("--username foo --password bar");

    let response = reqwest::blocking::Client::new()
        .get(format!("{}healthcheck", server.url()))
        .send()
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = reqwest::blocking::Client::new()
        .get(format!("{}healthcheck", server.url()))
        .basic_auth("foo", Some("bar"))
        .send()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
