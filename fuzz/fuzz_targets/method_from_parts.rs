#![no_main]

use {libfuzzer_sys::fuzz_target, serde_json::Value, stratum::Method};

fn float_free(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.as_i64().is_some() || number.as_u64().is_some(),
        Value::Array(values) => values.iter().all(float_free),
        Value::Object(map) => map.values().all(float_free),
        _ => true,
    }
}

fn exactly_roundtrips(method: &Method) -> bool {
    match method {
        Method::SetDifficulty(_) | Method::SuggestDifficulty(_) => false,
        Method::Configure(configure) => configure.minimum_difficulty_value.is_none(),
        Method::Unknown { params, .. } => float_free(params),
        _ => true,
    }
}

fuzz_target!(|data: &[u8]| {
    let Some(position) = data.iter().position(|&byte| byte == b'\n') else {
        return;
    };

    let Ok(method) = std::str::from_utf8(&data[..position]) else {
        return;
    };

    let Ok(params) = std::str::from_utf8(&data[position + 1..]) else {
        return;
    };

    let Ok(parsed) = Method::from_parts(method, params) else {
        return;
    };

    let mut buffer = Vec::new();

    parsed
        .serialize_params(&mut serde_json::Serializer::new(&mut buffer))
        .unwrap();

    let serialized = String::from_utf8(buffer).unwrap();

    let reparsed = Method::from_parts(parsed.method_name(), &serialized).unwrap();

    if exactly_roundtrips(&parsed) {
        assert_eq!(reparsed, parsed);
    }
});
