use super::*;

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
fn template_raw() {
    let ckpool = TestCkpool::spawn();

    let stratum_endpoint = ckpool.stratum_endpoint();

    let template = CommandBuilder::new(format!(
        "template {stratum_endpoint} --username {} --raw",
        signet_username()
    ))
    .spawn();

    let stdout = template.wait_with_output().unwrap();
    let output =
        serde_json::from_str::<Template>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    assert!(output.merkle_branches.is_empty());
    assert_eq!(output.extranonce2_size, 8);
    assert_eq!(output.ip_address, "127.0.0.1".to_string());

    assert_eq!(stdout.status.code(), Some(0));
}

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
fn template_interpreted() {
    let ckpool = TestCkpool::spawn();

    let stratum_endpoint = ckpool.stratum_endpoint();

    let template = CommandBuilder::new(format!(
        "template {stratum_endpoint} --username {}",
        signet_username()
    ))
    .spawn();

    let stdout = template.wait_with_output().unwrap();
    let output =
        serde_json::from_str::<InterpretedTemplate>(&String::from_utf8_lossy(&stdout.stdout))
            .unwrap();

    assert_eq!(output.mining_params.extranonce2_size, 8);
    assert_eq!(output.ip_address, "127.0.0.1".to_string());
    assert!(output.coinbase.block_height.is_some());
    assert!(output.block_header.difficulty > 0.0);

    assert_eq!(stdout.status.code(), Some(0));
}
