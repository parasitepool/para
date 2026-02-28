use super::*;

/// client.reconnect
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reconnect {
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub wait_time: Option<u32>,
}

impl Serialize for Reconnect {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.hostname.is_none() && self.port.is_none() && self.wait_time.is_none() {
            let seq = serializer.serialize_seq(Some(0))?;
            seq.end()
        } else {
            let mut seq = serializer.serialize_seq(Some(3))?;
            seq.serialize_element(self.hostname.as_deref().unwrap_or(""))?;
            seq.serialize_element(&self.port.unwrap_or(0))?;
            seq.serialize_element(&self.wait_time.unwrap_or(0))?;
            seq.end()
        }
    }
}

impl<'de> Deserialize<'de> for Reconnect {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values: Vec<Value> = Deserialize::deserialize(deserializer)?;

        let hostname = values
            .first()
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from);

        let port = values
            .get(1)
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .filter(|&p| p != 0);

        let wait_time = values
            .get(2)
            .and_then(Value::as_u64)
            .and_then(|w| u32::try_from(w).ok())
            .filter(|&w| w != 0);

        Ok(Reconnect {
            hostname,
            port,
            wait_time,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_serializes_to_empty_array() {
        let v = serde_json::to_value(Reconnect::default()).unwrap();
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn roundtrip() {
        #[track_caller]
        fn case(reconnect: Reconnect, json: &str) {
            let serialized = serde_json::to_string(&reconnect).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&serialized).unwrap(),
                serde_json::from_str::<Value>(json).unwrap(),
            );

            let back: Reconnect = serde_json::from_str(&serialized).unwrap();
            assert_eq!(back, reconnect);
        }

        case(Reconnect::default(), "[]");

        case(
            Reconnect {
                hostname: Some("foo".into()),
                port: Some(3333),
                wait_time: Some(5),
            },
            r#"["foo", 3333, 5]"#,
        );

        case(
            Reconnect {
                hostname: Some("bar".into()),
                port: None,
                wait_time: None,
            },
            r#"["bar", 0, 0]"#,
        );
    }

    #[test]
    fn deserialize_partial() {
        #[track_caller]
        fn case(json: &str, expected: Reconnect) {
            let parsed: Reconnect = serde_json::from_str(json).unwrap();
            assert_eq!(parsed, expected);
        }

        case(
            r#"["foo"]"#,
            Reconnect {
                hostname: Some("foo".into()),
                port: None,
                wait_time: None,
            },
        );

        case(
            r#"["foo", 3333]"#,
            Reconnect {
                hostname: Some("foo".into()),
                port: Some(3333),
                wait_time: None,
            },
        );
    }

    #[test]
    fn deserialize_zeroes_as_none() {
        let reconnect: Reconnect = serde_json::from_str(r#"["", 0, 0]"#).unwrap();
        assert_eq!(reconnect, Reconnect::default());
    }
}
