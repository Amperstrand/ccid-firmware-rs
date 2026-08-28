#![no_main]

use ccid_protocol::atr::{parse_atr, verify_atr_tck, AtrParams};
use libfuzzer_sys::fuzz_target;

// Fuzz the ccid-protocol ATR parser with arbitrary byte slices.
//
// Mirrors the random-input tests in `crates/ccid-protocol/src/atr.rs`
// (`test_parse_atr_random_bytes_no_panic`, `test_verify_tck_random_bytes_no_panic`):
// `parse_atr` and `verify_atr_tck` are total functions over arbitrary input —
// any panic here is a finding. `parse_atr` is a single bounded pass (no loops
// over attacker-controlled counters), so it cannot hang.
fuzz_target!(|data: &[u8]| {
    let params = parse_atr(data);

    // Inputs shorter than TS+T0 must fall back to the default parameters.
    if data.len() < 2 {
        assert_eq!(params, AtrParams::default());
    }

    // For T=1 ATRs long enough to carry a TCK, acceptance must be exactly
    // "the last byte equals the XOR of T0..TCK-1" (ISO 7816-3 section 8.2.4).
    if params.protocol == 1 && data.len() >= 3 {
        let expected: u8 = data[1..data.len() - 1].iter().fold(0u8, |acc, &b| acc ^ b);
        let tck_valid = expected == data[data.len() - 1];
        assert_eq!(
            verify_atr_tck(data, 1),
            tck_valid,
            "TCK verdict disagrees with its XOR definition"
        );
    }
});
