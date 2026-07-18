#![no_main]

use {libfuzzer_sys::fuzz_target, stratum::Difficulty};

fuzz_target!(|input: (f64, u64)| {
    let (float_difficulty, int_difficulty) = input;

    if float_difficulty.is_finite() && float_difficulty > 0.0 {
        let difficulty = Difficulty::from(float_difficulty);
        assert!(difficulty.as_f64().is_finite() && difficulty.as_f64() > 0.0);
    }

    if int_difficulty > 0 {
        let difficulty = Difficulty::from(int_difficulty);
        assert!(difficulty.as_f64().is_finite() && difficulty.as_f64() > 0.0);
    }
});
