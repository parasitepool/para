use super::*;

mod authorize;
mod configure;
mod notify;
mod reconnect;
mod set_difficulty;
mod submit;
mod subscribe;
mod suggest_difficulty;

pub use {
    authorize::Authorize,
    configure::{Configure, ConfigureResponse},
    notify::Notify,
    reconnect::Reconnect,
    set_difficulty::SetDifficulty,
    submit::Submit,
    subscribe::{Subscribe, SubscribeResponse},
    suggest_difficulty::SuggestDifficulty,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Method {
    Configure(Configure),
    Subscribe(Subscribe),
    Authorize(Authorize),
    Submit(Submit),
    Notify(Notify),
    SetDifficulty(SetDifficulty),
    SuggestDifficulty(SuggestDifficulty),
    Reconnect(Reconnect),
    Unknown { method: String, params: Value },
}

impl Method {
    pub fn method_name(&self) -> &str {
        match self {
            Self::Configure(_) => "mining.configure",
            Self::Subscribe(_) => "mining.subscribe",
            Self::Authorize(_) => "mining.authorize",
            Self::Submit(_) => "mining.submit",
            Self::Notify(_) => "mining.notify",
            Self::SetDifficulty(_) => "mining.set_difficulty",
            Self::SuggestDifficulty(_) => "mining.suggest_difficulty",
            Self::Reconnect(_) => "client.reconnect",
            Self::Unknown { method, .. } => method,
        }
    }

    pub fn serialize_params<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Configure(v) => v.serialize(serializer),
            Self::Subscribe(v) => v.serialize(serializer),
            Self::Authorize(v) => v.serialize(serializer),
            Self::Submit(v) => v.serialize(serializer),
            Self::Notify(v) => v.serialize(serializer),
            Self::SetDifficulty(v) => v.serialize(serializer),
            Self::SuggestDifficulty(v) => v.serialize(serializer),
            Self::Reconnect(v) => v.serialize(serializer),
            Self::Unknown { params, .. } => params.serialize(serializer),
        }
    }

    pub fn from_parts(method: &str, raw_params: &str) -> Result<Self, serde_json::Error> {
        match method {
            "mining.configure" => serde_json::from_str(raw_params).map(Self::Configure),
            "mining.subscribe" => serde_json::from_str(raw_params).map(Self::Subscribe),
            "mining.authorize" => serde_json::from_str(raw_params).map(Self::Authorize),
            "mining.submit" => serde_json::from_str(raw_params).map(Self::Submit),
            "mining.notify" => serde_json::from_str(raw_params).map(Self::Notify),
            "mining.set_difficulty" => serde_json::from_str(raw_params).map(Self::SetDifficulty),
            "mining.suggest_difficulty" => {
                serde_json::from_str(raw_params).map(Self::SuggestDifficulty)
            }
            "client.reconnect" => serde_json::from_str(raw_params).map(Self::Reconnect),
            _ => Ok(Self::Unknown {
                method: method.to_owned(),
                params: serde_json::from_str(raw_params)?,
            }),
        }
    }
}

#[cfg(test)]
impl Method {
    fn params_value(&self) -> serde_json::Result<Value> {
        match self {
            Self::Configure(v) => serde_json::to_value(v),
            Self::Subscribe(v) => serde_json::to_value(v),
            Self::Authorize(v) => serde_json::to_value(v),
            Self::Submit(v) => serde_json::to_value(v),
            Self::Notify(v) => serde_json::to_value(v),
            Self::SetDifficulty(v) => serde_json::to_value(v),
            Self::SuggestDifficulty(v) => serde_json::to_value(v),
            Self::Reconnect(v) => serde_json::to_value(v),
            Self::Unknown { params, .. } => Ok(params.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_parts() {
        #[track_caller]
        fn case(method_str: &str, raw_params: &str, expected_variant: &str) {
            let method = Method::from_parts(method_str, raw_params).unwrap();
            assert_eq!(method.method_name(), expected_variant);
        }

        case("mining.configure", "[[], {}]", "mining.configure");
        case("mining.subscribe", r#"["foo"]"#, "mining.subscribe");
        case("mining.authorize", r#"["foo"]"#, "mining.authorize");
        case("mining.set_difficulty", "[1]", "mining.set_difficulty");
        case(
            "mining.suggest_difficulty",
            "[1]",
            "mining.suggest_difficulty",
        );
        case("client.reconnect", "[]", "client.reconnect");
        case(
            "mining.notify",
            r#"["bf","4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000","aa","bb",[],"00000002","1c2ac4af","504e86b9",false]"#,
            "mining.notify",
        );
        case(
            "mining.submit",
            r#"["slush.miner1","bf","00000001","504e86ed","b2957c02"]"#,
            "mining.submit",
        );
    }

    #[test]
    fn unknown() {
        let method = Method::from_parts("mining.foo", "[1, 2]").unwrap();

        assert_eq!(method.method_name(), "mining.foo");
        assert_eq!(method.params_value().unwrap(), serde_json::json!([1, 2]));

        assert!(matches!(method, Method::Unknown { .. }));
    }

    #[test]
    fn from_parts_invalid_params() {
        assert!(Method::from_parts("mining.subscribe", "{}").is_err());
    }

    #[test]
    fn params_roundtrip() {
        #[track_caller]
        fn case(method: Method) {
            let name = method.method_name();
            let params = method.params_value().unwrap();
            let params = serde_json::to_string(&params).unwrap();
            let roundtripped = Method::from_parts(name, &params).unwrap();
            assert_eq!(method, roundtripped);
        }

        case(Method::SetDifficulty(SetDifficulty(Difficulty::from(42))));
        case(Method::SuggestDifficulty(SuggestDifficulty(
            Difficulty::from(1024),
        )));
        case(Method::Notify(Notify {
            job_id: "bf".parse().unwrap(),
            prevhash: "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000"
                .parse()
                .unwrap(),
            coinb1: "aa".into(),
            coinb2: "bb".into(),
            merkle_branches: Vec::new(),
            version: Version(block::Version::TWO),
            nbits: "1c2ac4af".parse().unwrap(),
            ntime: "504e86b9".parse().unwrap(),
            clean_jobs: false,
        }));
        case(Method::Submit(Submit {
            username: "foo".into(),
            job_id: "bf".parse().unwrap(),
            enonce2: "00000001".parse().unwrap(),
            ntime: "504e86ed".parse().unwrap(),
            nonce: "b2957c02".parse().unwrap(),
            version_bits: None,
        }));
        case(Method::Submit(Submit {
            username: "foo".into(),
            job_id: "bf".parse().unwrap(),
            enonce2: "00000001".parse().unwrap(),
            ntime: "504e86ed".parse().unwrap(),
            nonce: "b2957c02".parse().unwrap(),
            version_bits: Some("04d46000".parse().unwrap()),
        }));
        case(Method::Subscribe(Subscribe {
            user_agent: "foo".into(),
            enonce1: None,
        }));
        case(Method::Authorize(Authorize {
            username: "foo".into(),
            password: Some("bar".into()),
        }));
        case(Method::Configure(Configure {
            extensions: vec!["version-rolling".into()],
            minimum_difficulty_value: None,
            version_rolling_mask: Some("ffffffff".parse().unwrap()),
            version_rolling_min_bit_count: None,
        }));
        case(Method::Reconnect(Reconnect::default()));
        case(Method::Unknown {
            method: "mining.foo".into(),
            params: serde_json::json!([1, "bar"]),
        });
    }
}
