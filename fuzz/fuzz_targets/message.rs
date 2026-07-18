#![no_main]

use {
    libfuzzer_sys::fuzz_target,
    serde_json::Value,
    stratum::{MAX_MESSAGE_SIZE, Message, Method},
};

fn float_free(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.as_i64().is_some() || number.as_u64().is_some(),
        Value::Array(values) => values.iter().all(float_free),
        Value::Object(map) => map.values().all(float_free),
        _ => true,
    }
}

fn exactly_roundtrips(message: &Message) -> bool {
    match message {
        Message::Request { method, .. } | Message::Notification { method } => match method {
            Method::SetDifficulty(_) | Method::SuggestDifficulty(_) => false,
            Method::Configure(configure) => configure.minimum_difficulty_value.is_none(),
            Method::Unknown { params, .. } => float_free(params),
            _ => true,
        },
        Message::Response { result, error, .. } => {
            result.as_ref().is_none_or(float_free)
                && error
                    .as_ref()
                    .and_then(|error| error.traceback.as_ref())
                    .is_none_or(float_free)
        }
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_MESSAGE_SIZE {
        return;
    }

    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    let Ok(parsed) = serde_json::from_str::<Message>(input) else {
        return;
    };

    let serialized = serde_json::to_string(&parsed).unwrap();

    let reparsed = serde_json::from_str::<Message>(&serialized).unwrap();

    if exactly_roundtrips(&parsed) {
        assert_eq!(reparsed, parsed);
    }
});
