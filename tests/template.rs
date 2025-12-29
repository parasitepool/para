use {super::*, para::subcommand::template::Output};

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
    let output = serde_json::from_str::<Notify>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    assert!(output.merkle_branches.is_empty());
    assert!(output.clean_jobs); // Initial job should have clean_jobs=true

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
    let output = serde_json::from_str::<Output>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    assert!(output.network_difficulty > Difficulty::from(0.00001));
    assert!(output.clean_jobs);
    assert!(output.coinbase.size_bytes > 0);
    assert!(!output.coinbase.outputs.is_empty());

    assert!(output.ntime_human.contains('T'));
    assert!(output.ntime_human.ends_with('Z'));

    assert_eq!(stdout.status.code(), Some(0));
}
