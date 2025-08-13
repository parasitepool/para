use super::*;

fn zero_status() -> Status {
    Status {
        pool: PoolStatus {
            runtime: 0,
            lastupdate: 0,
            users: 0,
            workers: 0,
            idle: 0,
            disconnected: 0,
        },
        hash_rates: HashRateStatus {
            hashrate1m: HashRate(0.0),
            hashrate5m: HashRate(0.0),
            hashrate15m: HashRate(0.0),
            hashrate1hr: HashRate(0.0),
            hashrate6hr: HashRate(0.0),
            hashrate1d: HashRate(0.0),
            hashrate7d: HashRate(0.0),
        },
        shares: ShareStatus {
            diff: 0.0,
            accepted: 0,
            rejected: 0,
            bestshare: 0,
            sps1m: 0.0,
            sps5m: 0.0,
            sps15m: 0.0,
            sps1h: 0.0,
        },
    }
}

fn typical_status() -> Status {
    Status {
        pool: PoolStatus {
            runtime: 86400,
            lastupdate: 0,
            users: 5000,
            workers: 20000,
            idle: 1000,
            disconnected: 500,
        },
        hash_rates: HashRateStatus {
            hashrate1m: HashRate::from_str("100E").unwrap(),
            hashrate5m: HashRate::from_str("95E").unwrap(),
            hashrate15m: HashRate::from_str("98E").unwrap(),
            hashrate1hr: HashRate::from_str("102E").unwrap(),
            hashrate6hr: HashRate::from_str("99E").unwrap(),
            hashrate1d: HashRate::from_str("100E").unwrap(),
            hashrate7d: HashRate::from_str("105E").unwrap(),
        },
        shares: ShareStatus {
            diff: 120.5,
            accepted: 1_000_000_000_000,
            rejected: 10_000_000_000,
            bestshare: 500_000_000_000,
            sps1m: 5000.0,
            sps5m: 4900.0,
            sps15m: 4950.0,
            sps1h: 5050.0,
        },
    }
}

fn zero_user() -> User {
    User {
        hashrate1m: HashRate(0.0),
        hashrate5m: HashRate(0.0),
        hashrate1hr: HashRate(0.0),
        hashrate1d: HashRate(0.0),
        hashrate7d: HashRate(0.0),
        lastshare: 0,
        workers: 0,
        shares: 0,
        bestshare: 0.0,
        bestever: 0,
        authorised: 0,
        worker: vec![],
    }
}

fn typical_user() -> User {
    let now = 1755043200;
    User {
        hashrate1m: HashRate::from_str("10P").unwrap(),
        hashrate5m: HashRate::from_str("9.5P").unwrap(),
        hashrate1hr: HashRate::from_str("10.2P").unwrap(),
        hashrate1d: HashRate::from_str("10P").unwrap(),
        hashrate7d: HashRate::from_str("10.5P").unwrap(),
        lastshare: now - 60,
        workers: 3,
        shares: 1_000_000_000,
        bestshare: 1e12,
        bestever: 5_000_000_000,
        authorised: 1,
        worker: vec![
            Worker {
                workername: "rig1".to_string(),
                hashrate1m: HashRate::from_str("4P").unwrap(),
                hashrate5m: HashRate::from_str("3.8P").unwrap(),
                hashrate1hr: HashRate::from_str("4.1P").unwrap(),
                hashrate1d: HashRate::from_str("4P").unwrap(),
                hashrate7d: HashRate::from_str("4.2P").unwrap(),
                lastshare: now - 120,
                shares: 400_000_000,
                bestshare: 5e11,
                bestever: 2_000_000_000,
            },
            Worker {
                workername: "rig2".to_string(),
                hashrate1m: HashRate::from_str("3P").unwrap(),
                hashrate5m: HashRate::from_str("2.9P").unwrap(),
                hashrate1hr: HashRate::from_str("3.0P").unwrap(),
                hashrate1d: HashRate::from_str("3P").unwrap(),
                hashrate7d: HashRate::from_str("3.1P").unwrap(),
                lastshare: now - 180,
                shares: 300_000_000,
                bestshare: 3e11,
                bestever: 1_500_000_000,
            },
            Worker {
                workername: "rig3".to_string(),
                hashrate1m: HashRate::from_str("3P").unwrap(),
                hashrate5m: HashRate::from_str("2.8P").unwrap(),
                hashrate1hr: HashRate::from_str("3.1P").unwrap(),
                hashrate1d: HashRate::from_str("3P").unwrap(),
                hashrate7d: HashRate::from_str("3.2P").unwrap(),
                lastshare: now - 60,
                shares: 300_000_000,
                bestshare: 4e11,
                bestever: 1_500_000_000,
            },
        ],
    }
}

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
