use super::*;

#[test]
fn ping() {
    let ckpool = TestCkpool::spawn();

    let stratum_endpoint = ckpool.stratum_endpoint();

    let mut ping = CommandBuilder::new(format!("ping {stratum_endpoint}"))
        .stdout(false)
        .stderr(false)
        .spawn();

    thread::sleep(Duration::from_secs(5));

    ping.kill().unwrap();
    ping.wait().unwrap();
}
