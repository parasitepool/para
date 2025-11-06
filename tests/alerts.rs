use super::*;

#[derive(Debug, Deserialize, Serialize)]
struct NtfyMessage {
    id: String,
    time: i64,
    event: String,
    topic: String,
    message: Option<String>,
    title: Option<String>,
    priority: Option<u8>,
    tags: Option<Vec<String>>,
}

async fn listen_for_ntfy_message(channel: &str, timeout_duration: Duration) -> NtfyMessage {
    let client = reqwest::Client::new();
    let url = format!("https://ntfy.sh/{}/json?poll=1&since=30s", channel);

    let response = timeout(timeout_duration, client.get(&url).send())
        .await
        .unwrap()
        .unwrap();

    assert!(response.status().is_success());

    let text = response.text().await.unwrap();

    let messages: Vec<NtfyMessage> = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    messages
        .into_iter()
        .find(|msg| msg.event == "message")
        .unwrap()
}

fn generate_test_channel() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);

    let thread_id = std::thread::current().id();

    format!("test_para_{}_{}_{:?}", timestamp, counter, thread_id)
        .replace("ThreadId(", "")
        .replace(")", "")
}

#[test]
fn test_notification_priority_values() {
    assert_eq!(NotificationPriority::Max as u8, 5);
    assert_eq!(NotificationPriority::High as u8, 4);
    assert_eq!(NotificationPriority::Default as u8, 3);
    assert_eq!(NotificationPriority::Low as u8, 2);
    assert_eq!(NotificationPriority::Min as u8, 1);
}

#[test]
fn test_format_block_found_notification() {
    let handler = NotificationHandler::new("test_channel".to_string());
    let notification = NotificationType::BlockFound {
        height: 850000,
        hash: "00000000000000000002a7c4c1e48d76c5a37902165a270156b7a8d72728a054".to_string(),
        value: 625000000,
        miner: "test_pool".to_string(),
    };

    let (title, message, priority, tags) = handler.format_notification(notification);

    assert!(title.contains("850000"));
    assert!(title.contains("New Block Found"));
    assert!(message.contains("6.25000000 BTC"));
    assert!(message.contains("test_pool"));
    assert!(message.contains("850000"));
    assert!(matches!(priority, NotificationPriority::High));
    assert!(tags.contains(&"mining".to_string()));
    assert!(tags.contains(&"bitcoin".to_string()));
    assert!(tags.contains(&"pick".to_string()));
}

#[tokio::test]
#[ignore]
async fn test_send_block_notification() {
    let test_channel = generate_test_channel();
    let handler = NotificationHandler::new(test_channel.clone());

    let notification = NotificationType::BlockFound {
        height: 850000,
        hash: "00000000000000000002a7c4c1e48d76c5a37902165a270156b7a8d72728a054".to_string(),
        value: 625000000,
        miner: "test_pool".to_string(),
    };

    let send_result = handler.send(notification).await;
    assert!(
        send_result.is_ok(),
        "Failed to send notification: {:?}",
        send_result
    );

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let received_msg = listen_for_ntfy_message(&test_channel, Duration::from_secs(5)).await;

    assert!(received_msg.title.unwrap_or_default().contains("850000"));

    assert!(
        received_msg
            .message
            .unwrap_or_default()
            .contains("6.25000000 BTC")
    );

    assert_eq!(received_msg.priority, Some(4));

    if let Some(tags) = received_msg.tags {
        assert!(tags.iter().any(|t| t.contains("mining")));
    }
}

#[tokio::test]
async fn test_notification_failure_handling() {
    let handler = NotificationHandler::_with_custom_server(
        "http://invalid.ntfy.server.local".to_string(),
        "test_channel".to_string(),
    );

    let notification = NotificationType::SystemWarning {
        message: "This should fail".to_string(),
    };

    let result = handler.send(notification).await;
    assert!(result.is_err());
}
