use super::*;

#[test]
#[ignore]
fn miner() {
    let pool = TestPool::spawn();

    let stratum_endpoint = pool.stratum_endpoint();

    let mut miner = CommandBuilder::new(format!(
        "miner --once --username {} {stratum_endpoint}",
        signet_username()
    ))
    .spawn();

    let exit_status = miner.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}
