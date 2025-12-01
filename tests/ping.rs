use super::*;

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
fn ping_pool() {
    let pool = TestPool::spawn();

    let stratum_endpoint = pool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 10 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
fn ping_ckpool() {
    let ckpool = TestCkpool::spawn();

    let stratum_endpoint = ckpool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 1 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();

    assert_eq!(exit_status.code(), Some(0));

    let mut ping = CommandBuilder::new("ping --count 1 --timeout 1 127.0.0.1:1234").spawn();

    let exit_status = ping.wait().unwrap();

    assert_eq!(exit_status.code(), Some(1));

    let username = signet_username();

    let mut ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --username {username} {stratum_endpoint}"
    ))
    .spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));

    let mut ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --username {username} --password testpass {stratum_endpoint}"
    ))
    .spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));

    let mut ping =
        CommandBuilder::new("ping --count 1 --timeout 1 invalid.hostname.that.does.not.exist")
            .spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(1));

    let mut ping =
        CommandBuilder::new(format!("ping --count 3 --timeout 1 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}
