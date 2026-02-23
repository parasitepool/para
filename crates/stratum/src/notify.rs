use super::*;

#[derive(Debug, PartialEq, Clone)]
pub struct Notify {
    pub job_id: JobId,
    pub prevhash: PrevHash,
    pub coinb1: String,
    pub coinb2: String,
    pub merkle_branches: Vec<MerkleNode>,
    pub version: Version,
    pub nbits: Nbits,
    pub ntime: Ntime,
    pub clean_jobs: bool,
}

impl Serialize for Notify {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(9))?;
        seq.serialize_element(&self.job_id)?;
        seq.serialize_element(&self.prevhash)?;
        seq.serialize_element(&self.coinb1)?;
        seq.serialize_element(&self.coinb2)?;
        seq.serialize_element(&self.merkle_branches)?;
        seq.serialize_element(&self.version)?;
        seq.serialize_element(&self.nbits)?;
        seq.serialize_element(&self.ntime)?;
        seq.serialize_element(&self.clean_jobs)?;
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Notify {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (job_id, prevhash, coinb1, coinb2, merkle_branches, version, nbits, ntime, clean_jobs) =
            <(
                JobId,
                PrevHash,
                String,
                String,
                Vec<MerkleNode>,
                Version,
                Nbits,
                Ntime,
                bool,
            )>::deserialize(deserializer)?;

        Ok(Notify {
            job_id,
            prevhash,
            coinb1,
            coinb2,
            merkle_branches,
            version,
            nbits,
            ntime,
            clean_jobs,
        })
    }
}

#[cfg(test)]
mod tests {
    use {super::*, bitcoin::block};

    #[track_caller]
    fn case(json: &str, expected: Notify) {
        let parsed: Notify = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, expected, "deserialize equality");

        let ser = serde_json::to_string(&parsed).unwrap();
        let lhs: Value = serde_json::from_str(json).unwrap();
        let rhs: Value = serde_json::from_str(&ser).unwrap();
        assert_eq!(lhs, rhs, "semantic JSON equality");

        let back: Notify = serde_json::from_str(&ser).unwrap();
        assert_eq!(back, expected, "roundtrip equality");
    }

    fn sample_notify(clean_jobs: bool) -> Notify {
        Notify {
            job_id: "bf".parse().unwrap(),
            prevhash: "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000"
                .parse()
                .unwrap(),
            coinb1: "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020862062f503253482f04b8864e5008".into(),
            coinb2: "072f736c7573682f000000000100f2052a010000001976a914d23fcdf86f7e756a64a7a9688ef9903327048ed988ac00000000".into(),
            merkle_branches: Vec::new(),
            version: Version(block::Version::TWO),
            nbits: "1c2ac4af".parse().unwrap(),
            ntime: "504e86b9".parse().unwrap(),
            clean_jobs,
        }
    }

    #[test]
    fn notify_roundtrip_false() {
        let json = r#"[
            "bf",
            "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000",
            "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020862062f503253482f04b8864e5008",
            "072f736c7573682f000000000100f2052a010000001976a914d23fcdf86f7e756a64a7a9688ef9903327048ed988ac00000000",
            [],
            "00000002",
            "1c2ac4af",
            "504e86b9",
            false
        ]"#;

        case(json, sample_notify(false));
    }

    #[test]
    fn notify_serialize_shape() {
        let n = sample_notify(false);
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(
            v,
            serde_json::json!([
                "bf",
                "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000",
                "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020862062f503253482f04b8864e5008",
                "072f736c7573682f000000000100f2052a010000001976a914d23fcdf86f7e756a64a7a9688ef9903327048ed988ac00000000",
                [],
                "00000002",
                "1c2ac4af",
                "504e86b9",
                false
            ])
        );
    }

    #[test]
    fn notify_roundtrip_true() {
        let json = r#"[
            "bf",
            "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000",
            "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020862062f503253482f04b8864e5008",
            "072f736c7573682f000000000100f2052a010000001976a914d23fcdf86f7e756a64a7a9688ef9903327048ed988ac00000000",
            [],
            "00000002",
            "1c2ac4af",
            "504e86b9",
            true
        ]"#;

        case(json, sample_notify(true));
    }
}
