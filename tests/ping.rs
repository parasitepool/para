use super::*;

#[test]
#[serial(bitcoind)]
fn ping_local() {
    let pool = TestPool::spawn();

    let stratum_endpoint = pool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping {stratum_endpoint} --count 1 --timeout 10")).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
#[serial(bitcoind)]
fn ping_ckpool() {
    let ckpool = TestCkpool::spawn();

    let stratum_endpoint = ckpool.stratum_endpoint();

    let username = signet_username();

    assert_eq!(
        CommandBuilder::new(format!("ping {stratum_endpoint} --count 1 --timeout 1"))
            .spawn()
            .wait()
            .unwrap()
            .code(),
        Some(0)
    );

    assert_eq!(
        CommandBuilder::new("ping 127.0.0.1:1234 --count 1 --timeout 1")
            .spawn()
            .wait()
            .unwrap()
            .code(),
        Some(1)
    );

    assert_eq!(
        CommandBuilder::new(format!(
            "ping {stratum_endpoint} --count 1 --timeout 1 --username {username}"
        ))
        .spawn()
        .wait()
        .unwrap()
        .code(),
        Some(0)
    );

    assert_eq!(
        CommandBuilder::new(format!(
            "ping {stratum_endpoint} --count 1 --timeout 1 --username {username} --password testpass"
        ))
        .spawn()
        .wait()
        .unwrap()
        .code(),
        Some(0)
    );

    assert_eq!(
        CommandBuilder::new("ping invalid.hostname.that.does.not.exist --count 1 --timeout 1")
            .spawn()
            .wait()
            .unwrap()
            .code(),
        Some(1)
    );

    assert_eq!(
        CommandBuilder::new(format!("ping {stratum_endpoint} --count 3 --timeout 1"))
            .spawn()
            .wait()
            .unwrap()
            .code(),
        Some(0)
    );
}
