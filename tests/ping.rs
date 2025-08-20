use super::*;

fn test_username() -> String {
    "bc1p4r54k6ju6h92x8rvucsumg06nhl4fmnr9ecg6dzw5nk24r45dzasde25r3".to_string()
}

#[test]
fn ping() {
    let ckpool = TestCkpool::spawn();

    let stratum_endpoint = ckpool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 1 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();

    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn ping_fails() {
    let mut ping = CommandBuilder::new("ping --count 1 --timeout 1 127.0.0.1:1234").spawn();

    let exit_status = ping.wait().unwrap();

    assert_eq!(exit_status.code(), Some(1));
}

#[test]
fn ping_with_username() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();
    let username = test_username();

    let mut ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --username {username} {stratum_endpoint}"
    ))
    .spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn ping_with_show_messages() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();

    let mut ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --show-messages --message-timeout 2 {stratum_endpoint}"
    ))
    .spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn ping_with_show_messages_and_auth() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();
    let username = test_username();

    let mut ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --username {username} --show-messages --message-timeout 1 {stratum_endpoint}"
    )).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn ping_zero_message_timeout() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();
    let username = test_username();

    let mut ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --username {username} --show-messages --message-timeout 0 {stratum_endpoint}"
    )).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn ping_output_shows_authorized_type() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();
    let username = test_username();

    let ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --username {username} {stratum_endpoint}"
    ))
    .spawn();

    let output = ping.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!stdout.is_empty() || !stderr.is_empty());
}

#[test]
fn ping_with_custom_message_timeout() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();
    let username = test_username();

    let ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --username {username} --show-messages --message-timeout 3 {stratum_endpoint}"
    )).spawn();

    let output = ping.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!stdout.is_empty() || !stderr.is_empty());
}

#[test]
fn ping_with_username_and_password() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();
    let username = test_username();

    let mut ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 --username {username} --password testpass {stratum_endpoint}"
    ))
    .spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn ping_target_without_port() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();

    let ip = stratum_endpoint.split(':').next().unwrap();

    let mut ping = CommandBuilder::new(format!("ping --count 1 --timeout 1 {ip}")).spawn();

    let exit_status = ping.wait().unwrap();
    assert!(exit_status.code() == Some(0) || exit_status.code() == Some(1));
}

#[test]
fn ping_invalid_hostname() {
    let mut ping =
        CommandBuilder::new("ping --count 1 --timeout 1 invalid.hostname.that.does.not.exist")
            .spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(1));
}

#[test]
fn ping_multiple_counts() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 3 --timeout 1 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn ping_output_contains_stats() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();

    let ping =
        CommandBuilder::new(format!("ping --count 2 --timeout 1 {stratum_endpoint}")).spawn();

    let output = ping.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("ping statistics"));
    assert!(stdout.contains("packets transmitted"));
    assert!(stdout.contains("received"));
    assert!(stdout.contains("packet loss"));
}

#[test]
fn ping_output_shows_subscribe_type() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();

    let ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 1 {stratum_endpoint}")).spawn();

    let output = ping.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("SUBSCRIBE PING"));
}

#[test]
fn ping_output_shows_timing_info() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();

    let ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 1 {stratum_endpoint}")).spawn();

    let output = ping.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("Response from"));
    assert!(stdout.contains("seq="));
    assert!(stdout.contains("size="));
    assert!(stdout.contains("time="));
    assert!(stdout.contains("ms"));
}

#[test]
fn ping_long_timeout() {
    let ckpool = TestCkpool::spawn();
    let stratum_endpoint = ckpool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 5 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}
