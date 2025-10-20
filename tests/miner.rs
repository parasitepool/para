use super::*;

#[test]
#[ignore]
fn miner() {
    let pool = TestPool::spawn();

    let bitcoind = pool.bitcoind_handle();

    bitcoind.mine_blocks(16).unwrap();

    let stratum_endpoint = pool.stratum_endpoint();

    let miner = CommandBuilder::new(format!(
        "miner --once --username {} {stratum_endpoint}",
        signet_username()
    ))
    .spawn();

    let stdout = miner.wait_with_output().unwrap();
    let output =
        serde_json::from_str::<Vec<Share>>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    assert_eq!(output.len(), 1);
}
