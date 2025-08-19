use super::*;

#[test]
fn ping() {
    let ckpool = TestCkpool::spawn();

    let mut ping = CommandBuilder::new(format!(
        "ping --count 1 --timeout 1 {}",
        ckpool.stratum_endpoint()
    ))
    .spawn();

    let exit_status = ping.wait().unwrap();

    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn ping_fails() {
    let _ckpool = TestCkpool::spawn();

    let mut ping = CommandBuilder::new("ping --count 1 --timeout 1 127.0.0.1:1234").spawn();

    let exit_status = ping.wait().unwrap();

    assert_eq!(exit_status.code(), Some(1));
}
