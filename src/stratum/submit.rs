use super::*;

#[derive(Debug, PartialEq, Clone)]
pub struct Submit {
    pub username: String,
    pub job_id: String,
    pub extranonce2: String,
    pub ntime: Ntime,
    pub nonce: Nonce,
    pub version_bits: Option<Version>,
}

impl Serialize for Submit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let len = if self.version_bits.is_some() { 6 } else { 5 };
        let mut seq = serializer.serialize_seq(Some(len))?;
        seq.serialize_element(&self.username)?;
        seq.serialize_element(&self.job_id)?;
        seq.serialize_element(&self.extranonce2)?;
        seq.serialize_element(&self.ntime)?;
        seq.serialize_element(&self.nonce)?;
        if let Some(v) = &self.version_bits {
            seq.serialize_element(v)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Submit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Five((String, String, String, Ntime, Nonce)),
            Six((String, String, String, Ntime, Nonce, Option<Version>)),
        }

        match Raw::deserialize(deserializer)? {
            Raw::Five((username, job_id, extranonce2, ntime, nonce)) => Ok(Submit {
                username,
                job_id,
                extranonce2,
                ntime,
                nonce,
                version_bits: None,
            }),
            Raw::Six((username, job_id, extranonce2, ntime, nonce, version_bits)) => Ok(Submit {
                username,
                job_id,
                extranonce2,
                ntime,
                nonce,
                version_bits,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn case(json: &str, expected: Submit) {
        let parsed: Submit = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, expected, "deserialize equality");

        let ser = serde_json::to_string(&parsed).unwrap();
        let lhs: Value = serde_json::from_str(json).unwrap();
        let rhs: Value = serde_json::from_str(&ser).unwrap();
        assert_eq!(lhs, rhs, "semantic JSON equality");

        let back: Submit = serde_json::from_str(&ser).unwrap();
        assert_eq!(back, expected, "roundtrip equality");
    }

    #[test]
    fn submit_roundtrip_no_version_bits() {
        case(
            r#"["slush.miner1","bf","00000001","504e86ed","b2957c02"]"#,
            Submit {
                username: "slush.miner1".into(),
                job_id: "bf".into(),
                extranonce2: "00000001".into(),
                ntime: Ntime::from_str("504e86ed").unwrap(),
                nonce: Nonce::from_str("b2957c02").unwrap(),
                version_bits: None,
            },
        );
    }

    #[test]
    fn submit_roundtrip_with_version_bits() {
        case(
            r#"["slush.miner1","bf","00000001","504e86ed","b2957c02","04d46000"]"#,
            Submit {
                username: "slush.miner1".into(),
                job_id: "bf".into(),
                extranonce2: "00000001".into(),
                ntime: Ntime::from_str("504e86ed").unwrap(),
                nonce: Nonce::from_str("b2957c02").unwrap(),
                version_bits: Some(Version::from_str("04d46000").unwrap()),
            },
        );
    }

    #[test]
    fn submit_serialize_shape() {
        let a = Submit {
            username: "u".into(),
            job_id: "j".into(),
            extranonce2: "01".into(),
            ntime: Ntime::from_str("00000000").unwrap(),
            nonce: Nonce::from_str("00000000").unwrap(),
            version_bits: None,
        };
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::json!(["u", "j", "01", "00000000", "00000000"])
        );

        let b = Submit {
            version_bits: Some(Version::from_str("ffffffff").unwrap()),
            ..a.clone()
        };
        assert_eq!(
            serde_json::to_value(&b).unwrap(),
            serde_json::json!(["u", "j", "01", "00000000", "00000000", "ffffffff"])
        );
    }

    #[test]
    fn submit_reject_bad_arity() {
        assert!(serde_json::from_str::<Submit>(r#"["u","j","01","00000000"]"#).is_err());
        assert!(
            serde_json::from_str::<Submit>(
                r#"["u","j","01","00000000","00000000","ffffffff","extra"]"#
            )
            .is_err()
        );
    }
}
