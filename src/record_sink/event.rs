use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Share(ShareEvent),
    BlockFound(BlockFoundEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareEvent {
    pub timestamp: i64,
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
    pub timestamp: i64,
    pub blockheight: i32,
    pub blockhash: String,
    pub address: String,
    pub workername: String,
    pub diff: f64,
    pub coinbase_value: Option<i64>,
}

impl Event {
    pub fn _timestamp(&self) -> i64 {
        match self {
            Event::Share(e) => e.timestamp,
            Event::BlockFound(e) => e.timestamp,
        }
    }

    pub fn _event_type(&self) -> &'static str {
        match self {
            Event::Share(_) => "share",
            Event::BlockFound(_) => "block_found",
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::time::{SystemTime, UNIX_EPOCH},
    };

    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn test_share() -> Event {
        Event::Share(ShareEvent {
            timestamp: now(),
            address: "bc1test".into(),
            workername: "rig1".into(),
            pool_diff: 1.0,
            share_diff: 1.5,
            result: true,
            blockheight: Some(800000),
            reject_reason: None,
        })
    }

    fn test_block() -> Event {
        Event::BlockFound(BlockFoundEvent {
            timestamp: now(),
            blockheight: 800000,
            blockhash: "00000000000000000001".into(),
            address: "bc1test".into(),
            workername: "rig1".into(),
            diff: 1000.0,
            coinbase_value: Some(625000000),
        })
    }

    #[test]
    fn event_type_returns_correct_string() {
        assert_eq!(test_share()._event_type(), "share");
        assert_eq!(test_block()._event_type(), "block_found");
    }

    #[test]
    fn event_serializes_to_json() {
        let share = test_share();
        let json = serde_json::to_string(&share).unwrap();
        assert!(json.contains("\"type\":\"share\""));
    }

    #[test]
    fn event_deserializes_from_json() {
        let share = test_share();
        let json = serde_json::to_string(&share).unwrap();
        let parsed: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed._event_type(), "share");
    }
}
