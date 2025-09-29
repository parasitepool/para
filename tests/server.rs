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
    for i in 0..3 {
        let user = typical_user();
        let user_address = address(i);

        users.push((user_address.to_string(), user));
    }

    assert_eq!(users.len(), 3);

    let mut servers = Vec::new();
    for (address, user) in users.iter() {
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

    for (address, user) in users.iter() {
        let response = aggregator.get_json::<User>(format!("/aggregator/users/{address}"));
        pretty_assert_eq!(response, *user);
    }

    let users_response = aggregator.get_json::<Vec<String>>("/aggregator/users");

    assert_eq!(users_response.len(), users.len());

    assert_eq!(
        users_response.into_iter().collect::<HashSet<String>>(),
        users
            .into_iter()
            .map(|(address, _)| address)
            .collect::<HashSet<String>>()
    );
}

#[test]
fn status_json() {
    let server = TestServer::spawn();

    let status = server.get_json::<Status>("/status");

    assert!(status.disk_usage_percent > 0.0);
}

#[test]
fn status_with_auth() {
    let server = TestServer::spawn_with_args("--username foo --password bar");

    let response = reqwest::blocking::Client::new()
        .get(format!("{}status", server.url()))
        .send()
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = reqwest::blocking::Client::new()
        .get(format!("{}status", server.url()))
        .basic_auth("foo", Some("bar"))
        .send()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn aggregator_dashboard_with_auth() {
    let mut servers = Vec::new();
    for _ in 0..3 {
        let server = TestServer::spawn_with_args("--username foo --password bar");
        servers.push(server)
    }

    assert_eq!(servers.len(), 3);

    let aggregator = TestServer::spawn_with_args(format!(
        "--username foo --password bar --nodes {} --nodes {} --nodes {}",
        servers[0].url(),
        servers[1].url(),
        servers[2].url()
    ));

    let response = reqwest::blocking::Client::new()
        .get(format!("{}aggregator/dashboard", aggregator.url()))
        .send()
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = reqwest::blocking::Client::new()
        .get(format!("{}aggregator/dashboard", aggregator.url()))
        .basic_auth("foo", Some("bar"))
        .send()
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn aggregator_cache_ttl() {
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

        fs::write(
            server.log_dir().join("pool/pool.status"),
            typical_status().to_string(),
        )
        .unwrap();

        servers.push(server)
    }

    assert_eq!(servers.len(), 3);

    let aggregator = TestServer::spawn_with_args(format!(
        "--nodes {} --nodes {} --nodes {} --ttl 1",
        servers[0].url(),
        servers[1].url(),
        servers[2].url()
    ));

    aggregator.assert_response(
        "/aggregator/pool/pool.status",
        &(typical_status() + typical_status() + typical_status()).to_string(),
    );

    for (address, user) in users.iter().take(3) {
        let response = aggregator.get_json::<User>(format!("/aggregator/users/{address}"));
        pretty_assert_eq!(response, *user);
    }

    fs::write(
        servers[1].log_dir().join("pool/pool.status"),
        zero_status().to_string(),
    )
    .unwrap();

    fs::write(
        servers[0].log_dir().join(format!("users/{}", users[0].0)),
        serde_json::to_string(&zero_user()).unwrap(),
    )
    .unwrap();

    aggregator.assert_response(
        "/aggregator/pool/pool.status",
        &(typical_status() + typical_status() + typical_status()).to_string(),
    );

    let response = aggregator.get_json::<User>(format!("/aggregator/users/{}", users[0].0));
    pretty_assert_eq!(response, typical_user());

    thread::sleep(Duration::from_secs(1));

    aggregator.assert_response(
        "/aggregator/pool/pool.status",
        &(zero_status() + typical_status() + typical_status()).to_string(),
    );

    let response = aggregator.get_json::<User>(format!("/aggregator/users/{}", users[0].0));
    pretty_assert_eq!(response, zero_user());
}

#[test]
fn aggregator_negative_cache_on_users() {
    let mut servers = Vec::new();
    for _ in 0..3 {
        let server = TestServer::spawn();
        servers.push(server)
    }

    assert_eq!(servers.len(), 3);

    let aggregator = TestServer::spawn_with_args(format!(
        "--nodes {} --nodes {} --nodes {} --ttl 1",
        servers[0].url(),
        servers[1].url(),
        servers[2].url()
    ));

    let non_existent_user = "bc1ghostuser";

    aggregator.assert_response_code(
        format!("/aggregator/users/{non_existent_user}"),
        StatusCode::NOT_FOUND,
    );

    fs::write(
        servers[0]
            .log_dir()
            .join(format!("users/{non_existent_user}")),
        serde_json::to_string(&typical_user()).unwrap(),
    )
    .unwrap();

    aggregator.assert_response_code(
        format!("/aggregator/users/{non_existent_user}"),
        StatusCode::NOT_FOUND,
    );

    thread::sleep(Duration::from_secs(1));

    let response = aggregator.get_json::<User>(format!("/aggregator/users/{non_existent_user}"));
    pretty_assert_eq!(response, typical_user());
}

#[test]
#[ignore]
fn aggregator_cache_concurrent_pool_burst() {
    let mut servers = Vec::new();
    for _ in 0..3 {
        let server = TestServer::spawn();
        fs::write(
            server.log_dir().join("pool/pool.status"),
            typical_status().to_string(),
        )
        .unwrap();

        servers.push(server);
    }

    let aggregator = Arc::new(TestServer::spawn_with_args(format!(
        "--nodes {} --nodes {} --nodes {} --ttl 1",
        servers[0].url(),
        servers[1].url(),
        servers[2].url(),
    )));

    aggregator.assert_response(
        "/aggregator/pool/pool.status",
        &(typical_status() + typical_status() + typical_status()).to_string(),
    );

    fs::write(
        servers[1].log_dir().join("pool/pool.status"),
        zero_status().to_string(),
    )
    .unwrap();

    const N: usize = 100;
    let start = Arc::new(Barrier::new(N + 1));
    let expected_old = (typical_status() + typical_status() + typical_status()).to_string();

    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let agg = aggregator.clone();
        let go = start.clone();
        let exp = expected_old.clone();
        handles.push(thread::spawn(move || {
            go.wait();
            agg.assert_response("/aggregator/pool/pool.status", &exp);
        }));
    }

    start.wait();

    for handle in handles {
        handle.join().unwrap();
    }

    thread::sleep(Duration::from_secs(1));

    let expected_new = (zero_status() + typical_status() + typical_status()).to_string();
    let start = Arc::new(Barrier::new(N + 1));
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let agg = aggregator.clone();
        let go = start.clone();
        let exp = expected_new.clone();
        handles.push(thread::spawn(move || {
            go.wait();
            agg.assert_response("/aggregator/pool/pool.status", &exp);
        }));
    }

    start.wait();

    for handles in handles {
        handles.join().unwrap();
    }
}

#[test]
#[ignore]
fn aggregator_cache_concurrent_user_burst() {
    let mut users = Vec::new();
    for i in 0..9 {
        let user = typical_user();
        let user_address = address(i);

        users.push((user_address.to_string(), user));
    }

    let mut servers = Vec::new();
    for (address, user) in users.iter().take(3) {
        let server = TestServer::spawn();
        fs::create_dir_all(server.log_dir().join("users")).unwrap();
        fs::write(
            server.log_dir().join(format!("users/{address}")),
            serde_json::to_string(&user).unwrap(),
        )
        .unwrap();
        servers.push(server);
    }

    let aggregator = Arc::new(TestServer::spawn_with_args(format!(
        "--nodes {} --nodes {} --nodes {} --ttl 1",
        servers[0].url(),
        servers[1].url(),
        servers[2].url(),
    )));

    let u0 = aggregator.get_json::<User>(format!("/aggregator/users/{}", users[0].0));
    pretty_assert_eq!(u0, typical_user());

    fs::write(
        servers[0].log_dir().join(format!("users/{}", users[0].0)),
        serde_json::to_string(&zero_user()).unwrap(),
    )
    .unwrap();

    const N: usize = 100;
    let start = Arc::new(Barrier::new(N + 1));
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let agg = aggregator.clone();
        let go = start.clone();
        let addr = users[0].0.clone();
        handles.push(thread::spawn(move || {
            go.wait();
            let got = agg.get_json::<User>(format!("/aggregator/users/{addr}"));
            pretty_assert_eq!(got, typical_user());
        }));
    }

    start.wait();

    for handle in handles {
        handle.join().unwrap();
    }

    thread::sleep(Duration::from_secs(1));

    let start = Arc::new(Barrier::new(N + 1));
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let agg = aggregator.clone();
        let go = start.clone();
        let addr = users[0].0.clone();
        handles.push(thread::spawn(move || {
            go.wait();
            let got = agg.get_json::<User>(format!("/aggregator/users/{addr}"));
            pretty_assert_eq!(got, zero_user());
        }));
    }

    start.wait();

    for handle in handles {
        handle.join().unwrap();
    }
}
