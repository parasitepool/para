#![no_main]

use {
    libfuzzer_sys::fuzz_target,
    std::{fmt::Debug, str::FromStr},
    stratum::{
        Difficulty, Extranonce, JobId, MerkleNode, Nbits, Nonce, Ntime, PrevHash, Username,
        Version, parse_si,
    },
};

fn roundtrip<T>(input: &str)
where
    T: FromStr + ToString + PartialEq + Debug,
    T::Err: Debug,
{
    if let Ok(value) = input.parse::<T>() {
        assert_eq!(value.to_string().parse::<T>().unwrap(), value);
    }
}

fuzz_target!(|input: &str| {
    roundtrip::<Nonce>(input);
    roundtrip::<Ntime>(input);
    roundtrip::<JobId>(input);
    roundtrip::<Version>(input);
    roundtrip::<Nbits>(input);
    roundtrip::<Extranonce>(input);
    roundtrip::<PrevHash>(input);
    roundtrip::<MerkleNode>(input);
    roundtrip::<Username>(input);

    if let Ok(difficulty) = input.parse::<Difficulty>() {
        let _ = difficulty.as_f64();
        let _ = difficulty.to_target();
        serde_json::to_string(&difficulty).unwrap();
    }

    let _ = parse_si(input, &["H/s", "H", "Hd"]);
});
