use super::*;

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
fn template_raw() {
    use para::stratum::Notify;

    let ckpool = TestCkpool::spawn();

    let stratum_endpoint = ckpool.stratum_endpoint();

    let template = CommandBuilder::new(format!(
        "template {stratum_endpoint} --username {} --raw",
        signet_username()
    ))
    .spawn();

    let stdout = template.wait_with_output().unwrap();
    let output = serde_json::from_str::<Notify>(&String::from_utf8_lossy(&stdout.stdout)).unwrap();

    // Notify is a JSON array: [job_id, prevhash, coinb1, coinb2, merkle_branches, version, nbits, ntime, clean_jobs]
    assert!(output.merkle_branches.is_empty());
    assert!(output.clean_jobs); // Initial job should have clean_jobs=true

    assert_eq!(stdout.status.code(), Some(0));
}

#[test]
#[serial(bitcoind)]
#[timeout(90000)]
fn template_interpreted() {
    use para::subcommand::template::InterpretedOutput;

    let ckpool = TestCkpool::spawn();

    let stratum_endpoint = ckpool.stratum_endpoint();

    let template = CommandBuilder::new(format!(
        "template {stratum_endpoint} --username {}",
        signet_username()
    ))
    .spawn();

    let stdout = template.wait_with_output().unwrap();
    let output =
        serde_json::from_str::<InterpretedOutput>(&String::from_utf8_lossy(&stdout.stdout))
            .unwrap();

    assert!(output.difficulty > 0.0);
    assert!(output.clean_jobs);

    // Verify timestamp is ISO 8601 format (contains T and ends with Z)
    let ts = &output.ntime_human;
    assert!(ts.contains('T'), "timestamp missing T: {}", ts);
    assert!(ts.trim().ends_with('Z'), "timestamp missing Z: {}", ts);

    // Verify merkle root is present and valid hex (64 chars)
    assert_eq!(output.merkle_root.len(), 64);

    // Verify version_info is populated
    assert!(output.version_info.bits > 0);

    assert_eq!(stdout.status.code(), Some(0));
}
