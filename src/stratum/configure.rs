use super::*;

/// Response from mining.configure method
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
pub struct ConfigureResponse {
    #[serde(rename = "version-rolling", default)]
    pub version_rolling: bool,

    #[serde(rename = "version-rolling.mask", default)]
    pub version_rolling_mask: Option<Version>,

    #[serde(rename = "minimum-difficulty", default)]
    pub minimum_difficulty: bool,
}

#[derive(Debug, PartialEq, Clone)]
pub struct Configure {
    pub extensions: Vec<String>,
    pub minimum_difficulty_value: Option<Difficulty>,
    pub version_rolling_mask: Option<Version>,
    pub version_rolling_min_bit_count: Option<u32>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct ConfigureOptions {
    #[serde(
        rename = "minimum-difficulty.value",
        skip_serializing_if = "Option::is_none"
    )]
    minimum_difficulty_value: Option<Difficulty>,

    #[serde(
        rename = "version-rolling.mask",
        skip_serializing_if = "Option::is_none"
    )]
    version_rolling_mask: Option<Version>,

    #[serde(
        rename = "version-rolling.min-bit-count",
        skip_serializing_if = "Option::is_none"
    )]
    version_rolling_min_bit_count: Option<u32>,
}

impl Serialize for Configure {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let opts = ConfigureOptions {
            minimum_difficulty_value: self.minimum_difficulty_value,
            version_rolling_mask: self.version_rolling_mask,
            version_rolling_min_bit_count: self.version_rolling_min_bit_count,
        };

        (&self.extensions, &opts).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Configure {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One((Vec<String>,)),
            Two((Vec<String>, ConfigureOptions)),
        }

        match Raw::deserialize(deserializer)? {
            Raw::One((extensions,)) => Ok(Configure {
                extensions,
                minimum_difficulty_value: None,
                version_rolling_mask: None,
                version_rolling_min_bit_count: None,
            }),
            Raw::Two((extensions, opts)) => Ok(Configure {
                extensions,
                minimum_difficulty_value: opts.minimum_difficulty_value,
                version_rolling_mask: opts.version_rolling_mask,
                version_rolling_min_bit_count: opts.version_rolling_min_bit_count,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use {super::*, serde::de::DeserializeOwned};

    #[track_caller]
    fn case<T>(json: &str, expected: T)
    where
        T: DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
    {
        let parsed: T = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, expected, "deserialize equality");

        let ser = serde_json::to_string(&parsed).unwrap();
        let lhs: Value = serde_json::from_str(json).unwrap();
        let rhs: Value = serde_json::from_str(&ser).unwrap();
        assert_eq!(lhs, rhs, "semantic JSON equality");

        let back: T = serde_json::from_str(&ser).unwrap();
        assert_eq!(back, expected, "roundtrip equality");
    }

    #[test]
    fn configure_deserialize_one_element_normalizes() {
        let cfg: Configure = serde_json::from_str(r#"[["version-rolling"]]"#).unwrap();
        assert_eq!(
            cfg,
            Configure {
                extensions: vec!["version-rolling".into()],
                minimum_difficulty_value: None,
                version_rolling_mask: None,
                version_rolling_min_bit_count: None,
            }
        );
        let v = serde_json::to_value(&cfg).unwrap();
        assert_eq!(v, serde_json::json!([["version-rolling"], {}]));
    }

    #[test]
    fn configure_with_mask_roundtrip() {
        case::<Configure>(
            r#"[["version-rolling"],{"version-rolling.mask":"ffffffff"}]"#,
            Configure {
                extensions: vec!["version-rolling".into()],
                minimum_difficulty_value: None,
                version_rolling_mask: Some(Version::from_str("ffffffff").unwrap()),
                version_rolling_min_bit_count: None,
            },
        );
    }

    #[test]
    fn configure_with_all_options_roundtrip() {
        case::<Configure>(
            r#"[["minimum-difficulty","version-rolling"],{"minimum-difficulty.value":2048,"version-rolling.mask":"00fff000","version-rolling.min-bit-count":2}]"#,
            Configure {
                extensions: vec!["minimum-difficulty".into(), "version-rolling".into()],
                minimum_difficulty_value: Some(Difficulty::from(2048)),
                version_rolling_mask: Some(Version::from_str("00fff000").unwrap()),
                version_rolling_min_bit_count: Some(2),
            },
        );
    }

    #[test]
    fn configure_serialize_shape_includes_only_present_fields() {
        let cfg = Configure {
            extensions: vec!["minimum-difficulty".into(), "version-rolling".into()],
            minimum_difficulty_value: Some(Difficulty::from(1024)),
            version_rolling_mask: None,
            version_rolling_min_bit_count: Some(3),
        };
        let v = serde_json::to_value(&cfg).unwrap();
        assert_eq!(
            v,
            serde_json::json!([
                ["minimum-difficulty","version-rolling"],
                {
                    "minimum-difficulty.value": 1024,
                    "version-rolling.min-bit-count": 3
                }
            ])
        );
    }

    #[test]
    fn configure_unknown_keys_are_ignored() {
        let cfg: Configure = serde_json::from_str(
            r#"[["version-rolling"],{"version-rolling.mask":"00000001","unknown":123}]"#,
        )
        .unwrap();
        assert_eq!(
            cfg,
            Configure {
                extensions: vec!["version-rolling".into()],
                minimum_difficulty_value: None,
                version_rolling_mask: Some(Version::from_str("00000001").unwrap()),
                version_rolling_min_bit_count: None,
            }
        );
        let v = serde_json::to_value(&cfg).unwrap();
        assert_eq!(
            v,
            serde_json::json!([["version-rolling"], {"version-rolling.mask":"00000001"}])
        );
    }
}
