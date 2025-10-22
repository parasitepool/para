use super::*;

#[test]
fn ping_pool() {
    let pool = TestPool::spawn();

    let stratum_endpoint = pool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 10 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn configure_template_update_interval() {
    let pool = TestPool::spawn_with_args("--update-interval 1");

    let stratum_endpoint = pool.stratum_endpoint();

    let output = CommandBuilder::new(format!(
        "template {stratum_endpoint} --username {}",
        signet_username()
    ))
    .spawn()
    .wait_with_output()
    .unwrap();

    let t1 = serde_json::from_str::<Template>(&String::from_utf8_lossy(&output.stdout)).unwrap();

    std::thread::sleep(Duration::from_secs(1));

    let output = CommandBuilder::new(format!(
        "template {stratum_endpoint} --username {}",
        signet_username()
    ))
    .spawn()
    .wait_with_output()
    .unwrap();

    let t2 = serde_json::from_str::<Template>(&String::from_utf8_lossy(&output.stdout)).unwrap();

    assert!(t1.ntime < t2.ntime);
}

#[tokio::test]
async fn basic_initialization_flow() {
    let pool = TestPool::spawn();

    let mut client = pool.stratum_client().await;

    let (subscribe, _, _) = client.subscribe().await.unwrap();

    assert_eq!(subscribe.subscriptions.len(), 2);

    assert!(client.authorize().await.is_ok());

    let set_difficulty = match client.incoming.recv().await.unwrap() {
        Message::Notification { method: _, params } => {
            serde_json::from_value::<SetDifficulty>(params).unwrap()
        }
        _ => panic!(),
    };

    assert!(set_difficulty.difficulty() < Difficulty::from(0.01));

    let notify = match client.incoming.recv().await.unwrap() {
        Message::Notification { method: _, params } => {
            serde_json::from_value::<Notify>(params).unwrap()
        }
        _ => panic!(),
    };

    assert_eq!(notify.job_id, JobId::from(0));
    assert!(notify.clean_jobs);
}

#[tokio::test]
async fn configure_with_multiple_negotiation_steps() {
    let pool = TestPool::spawn();

    let mut client = pool.stratum_client().await;

    assert!(
        client
            .configure(vec!["unknown-extension".into()], None)
            .await
            .unwrap_err()
            .to_string()
            .contains("Unsupported extension")
    );

    assert!(
        client
            .configure(
                vec!["version-rolling".into()],
                Some(Version::from_str("1fffe000").unwrap())
            )
            .await
            .is_ok()
    );

    assert!(
        client
            .configure(
                vec!["version-rolling".into()],
                Some(Version::from_str("1fffe111").unwrap())
            )
            .await
            .is_ok()
    );

    let (subscribe, _, _) = client.subscribe().await.unwrap();

    assert_eq!(subscribe.subscriptions.len(), 2);

    assert!(client.authorize().await.is_ok());
}

#[tokio::test]
async fn authorize_before_subscribe_fails() {
    let pool = TestPool::spawn();

    let mut client = pool.stratum_client().await;

    assert!(
        client
            .authorize()
            .await
            .unwrap_err()
            .to_string()
            .contains("Method not allowed in current state")
    );
}

#[tokio::test]
async fn submit_before_authorize_fails() {
    let pool = TestPool::spawn();

    let mut client = pool.stratum_client().await;

    client.subscribe().await.unwrap();

    assert!(
        client
            .submit(
                JobId::new(3),
                Extranonce::generate(8),
                Ntime::from(0),
                Nonce::from(12345),
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("Unauthorized")
    );
}
