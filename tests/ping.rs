use super::*;

#[test]
#[serial(bitcoind)]
fn ping_with_ckpool() {
    let ckpool = TestCkpool::spawn();

    let stratum_endpoint = ckpool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 1 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();

    assert_eq!(exit_status.code(), Some(0));
}

#[test]
#[serial(bitcoind)]
fn ping_fails_with_ckpool() {
    let _ = TestCkpool::spawn();

    let mut ping = CommandBuilder::new("ping --count 1 --timeout 1 127.0.0.1:1234").spawn();

    let exit_status = ping.wait().unwrap();

    assert_eq!(exit_status.code(), Some(1));
}

#[test]
#[serial(bitcoind)]
fn ping_with_username_with_ckpool() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();
    let username = signet_username();

    let mut ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --username {username} {stratum_endpoint}"
    ))
    .spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
#[serial(bitcoind)]
fn ping_with_username_and_password_with_ckpool() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();
    let username = signet_username();

    let mut ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --username {username} --password testpass {stratum_endpoint}"
    ))
    .spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
#[serial(bitcoind)]
fn ping_invalid_hostname_fails_with_ckpool() {
    let mut ping =
        CommandBuilder::new("ping --count 1 --timeout 1 invalid.hostname.that.does.not.exist")
            .spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(1));
}

#[test]
#[serial(bitcoind)]
fn ping_multiple_counts_with_ckpool() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 3 --timeout 1 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
#[serial(bitcoind)]
fn ping_output_comprehensive_with_ckpool() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();

    let ping =
        CommandBuilder::new(format!("ping --count 2 --timeout 1 {stratum_endpoint}")).spawn();

    let output = ping.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("SUBSCRIBE PING"));

    assert!(stdout.contains("Response from"));
    assert!(stdout.contains("seq="));
    assert!(stdout.contains("size="));
    assert!(stdout.contains("time="));
    assert!(stdout.contains("ms"));

    assert!(stdout.contains("ping statistics"));
    assert!(stdout.contains("packets transmitted"));
    assert!(stdout.contains("received"));
    assert!(stdout.contains("packet loss"));

    assert_eq!(output.status.code(), Some(0));
}
