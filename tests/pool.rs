use super::*;

#[test]
fn pool_is_pingable() {
    let pool = TestPool::spawn();

    let stratum_endpoint = pool.stratum_endpoint();

    let mut ping =
        CommandBuilder::new(format!("ping --count 1 --timeout 10 {stratum_endpoint}")).spawn();

    let exit_status = ping.wait().unwrap();
    assert_eq!(exit_status.code(), Some(0));
}

#[test]
fn pool_update_interval() {
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

// TODO:
// sending multiple configure at beginning is allowed for negotation
// sending submit before authorize fails
// sending authorize before subscribe fails
// unknown method return something?
//

#[tokio::test]
async fn stratum_happy_path() {
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
async fn stratum_some_errors() {
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

    let (subscribe, _, _) = client.subscribe().await.unwrap();

    assert_eq!(subscribe.subscriptions.len(), 2);

    assert!(client.authorize().await.is_ok());
}
