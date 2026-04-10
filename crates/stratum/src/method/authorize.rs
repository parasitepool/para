use super::*;

/// mining.authorize
#[derive(Debug, Clone, PartialEq)]
pub struct Authorize {
    pub username: Username,
    pub password: Option<String>,
}

impl Serialize for Authorize {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let len = if self.password.is_some() { 2 } else { 1 };
        let mut seq = serializer.serialize_seq(Some(len))?;
        seq.serialize_element(&self.username)?;
        if let Some(pass) = &self.password {
            seq.serialize_element(pass)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Authorize {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One((String,)),
            Two((String, Option<String>)),
        }

        match Raw::deserialize(deserializer)? {
            Raw::One((username,)) => Ok(Authorize {
                username: Username::try_from(username).map_err(serde::de::Error::custom)?,
                password: None,
            }),
            Raw::Two((username, password)) => Ok(Authorize {
                username: Username::try_from(username).map_err(serde::de::Error::custom)?,
                password,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn case(json: &str, expected: Authorize) {
        let parsed: Authorize = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, expected, "deserialize equality");

        let ser = serde_json::to_string(&parsed).unwrap();
        let lhs: Value = serde_json::from_str(json).unwrap();
        let rhs: Value = serde_json::from_str(&ser).unwrap();
        assert_eq!(lhs, rhs, "semantic JSON equality");

        let back: Authorize = serde_json::from_str(&ser).unwrap();
        assert_eq!(back, expected, "roundtrip equality");
    }

    #[test]
    fn authorize_with_password_roundtrip() {
        case(
            r#"["bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.miner1","password"]"#,
            Authorize {
                username: "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.miner1"
                    .parse()
                    .unwrap(),
                password: Some("password".into()),
            },
        );
    }

    #[test]
    fn authorize_omitted_password_roundtrip() {
        let expected = Authorize {
            username: "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.default"
                .parse()
                .unwrap(),
            password: None,
        };
        let parsed: Result<Authorize, _> =
            serde_json::from_str(r#"["bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.default"]"#);
        let parsed = parsed.expect("should accept omitted password");

        assert_eq!(parsed, expected);

        let v = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            v,
            serde_json::json!(["bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.default"])
        );
    }

    #[test]
    fn authorize_null_password_normalizes() {
        let parsed: Authorize =
            serde_json::from_str(r#"["bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.default",null]"#)
                .unwrap();
        assert_eq!(
            parsed,
            Authorize {
                username: "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.default"
                    .parse()
                    .unwrap(),
                password: None,
            }
        );

        let v = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            v,
            serde_json::json!(["bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.default"])
        );
    }

    #[test]
    fn authorize_reject_bad_arity() {
        assert!(
            serde_json::from_str::<Authorize>(r#"[]"#).is_err(),
            "empty array should error"
        );
        assert!(
            serde_json::from_str::<Authorize>(r#"["u","p","extra"]"#).is_err(),
            "extra elements should error"
        );
    }
}
