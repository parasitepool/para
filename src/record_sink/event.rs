use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Share(ShareEvent),
    BlockFound(BlockFoundEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
    pub address: String,
    pub workername: String,
    pub pool_diff: f64,
    pub share_diff: f64,
    pub result: bool,
    pub blockheight: Option<i32>,
    pub reject_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockFoundEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
    pub blockheight: i32,
    pub blockhash: String,
    pub address: String,
    pub workername: String,
    pub diff: f64,
    pub coinbase_value: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_share() -> Event {
        Event::Share(ShareEvent {
            timestamp: None,
            address: "bc1test".into(),
            workername: "rig1".into(),
            pool_diff: 1.0,
            share_diff: 1.5,
            result: true,
            blockheight: Some(800000),
            reject_reason: None,
        })
    }

    #[test]
    fn event_serializes_to_json() {
        let share = test_share();
        let json = serde_json::to_string(&share).unwrap();
        assert!(json.contains("\"type\":\"share\""));
    }
}
