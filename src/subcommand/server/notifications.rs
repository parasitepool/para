use super::*;

#[derive(Debug, Clone)]
pub enum NotificationType {
    BlockFound {
        height: i32,
        hash: String,
        value: i64,
        miner: String,
    },
    #[allow(dead_code)]
    SystemWarning { message: String },
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum NotificationPriority {
    Max = 5,
    High = 4,
    Default = 3,
    Low = 2,
    Min = 1,
}

pub struct NotificationHandler {
    ntfy_url: String,
    channel: String,
    client: reqwest::Client,
}

impl NotificationHandler {
    pub fn new(channel: String) -> Self {
        Self {
            ntfy_url: "https://ntfy.sh".to_string(),
            channel,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    pub fn _with_custom_server(server_url: String, channel: String) -> Self {
        Self {
            ntfy_url: server_url,
            channel,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    pub async fn send(&self, notification: NotificationType) -> Result<()> {
        let (title, message, priority, tags) = self.format_notification(notification);

        self.send_raw(title, message, priority, tags).await
    }

    pub async fn send_raw(
        &self,
        title: String,
        message: String,
        priority: NotificationPriority,
        tags: Vec<String>,
    ) -> Result<()> {
        let url = format!("{}/{}", self.ntfy_url, self.channel);

        let mut request = self
            .client
            .post(&url)
            .header("Title", title)
            .header("Priority", (priority as u8).to_string())
            .body(message.clone());

        if !tags.is_empty() {
            request = request.header("Tags", tags.join(","));
        }

        match request.send().await {
            Ok(response) => {
                if response.status().is_success() {
                    info!(
                        "Notification sent successfully to ntfy channel: {}",
                        self.channel
                    );
                    Ok(())
                } else {
                    let status = response.status();
                    let error_body = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error".to_string());
                    error!(
                        "Failed to send notification. Status: {}, Body: {}",
                        status, error_body
                    );
                    Err(anyhow!("Failed to send notification: {}", status))
                }
            }
            Err(e) => {
                error!("Failed to send notification to ntfy: {}", e);
                Err(anyhow!("Failed to send notification: {}", e))
            }
        }
    }

    pub fn format_notification(
        &self,
        notification: NotificationType,
    ) -> (String, String, NotificationPriority, Vec<String>) {
        match notification {
            NotificationType::BlockFound {
                height,
                hash,
                value,
                miner,
            } => {
                let btc_value = value as f64 / 100_000_000.0;
                (
                    format!("⛏️ New Block Found! #{}", height),
                    format!(
                        "Block Height: {}\nHash: {}\nValue: {:.8} BTC\nMiner: {}",
                        height,
                        &hash[..16], // might be backwards? can't remember, need better test case
                        btc_value,
                        miner
                    ),
                    NotificationPriority::High,
                    vec![
                        "pick".to_string(),
                        "bitcoin".to_string(),
                        "mining".to_string(),
                    ],
                )
            }
            NotificationType::SystemWarning { message } => (
                format!("System Warning! #{}", message),
                format!("System Warning: {}", message),
                NotificationPriority::Default,
                Vec::new(),
            ),
        }
    }

    pub async fn _send_test(&self) -> Result<()> {
        self.send_raw(
            "🔔 Test Notification".to_string(),
            "This is a test notification from the parasite pool server.".to_string(),
            NotificationPriority::Low,
            vec!["test".to_string()],
        )
        .await
    }
}

pub async fn notify_block_found(
    alerts_ntfy_channel: &Option<String>,
    height: i32,
    hash: String,
    value: i64,
    miner: String,
) -> Result<()> {
    if let Some(alerts_ntfy_channel) = alerts_ntfy_channel {
        let handler = NotificationHandler::new(alerts_ntfy_channel.clone());

        handler
            .send(NotificationType::BlockFound {
                height,
                hash,
                value,
                miner,
            })
            .await
    } else {
        Err(anyhow!("No alerts ntfy channel to notify"))
    }
}
