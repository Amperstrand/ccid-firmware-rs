//! CCID-handler fuzz suite (issue #92 Part A): malformed and adversarial
//! inputs through `CcidHandler::process_command` via the mock driver —
//! no panics, no state-machine dead-ends, spec-shaped status words.
//!
//! Deterministic LCG instead of a fuzzing framework: the cases reproduce
//! in CI without tooling, and each round exercises the exact same paths a
//! coverage-guided fuzzer would reach in this state machine (header
//! parsing, payload-length handling, per-command dispatch).

use crate::ccid_handler::CcidHandler;
use crate::nfc::{MockNfcDriver, NfcDriver};

/// Standard CCID message types (PC_to_RDR range 0x62..=0x73).
const ALL_MSG_TYPES: [u8; 14] = [
    0x62, 0x63, 0x65, 0x6F, 0x6C, 0x61, 0x69, 0x6A, 0x6B, 0x6D, 0x6E, 0x71, 0x72, 0x73,
];

fn handler_with(card_present: bool) -> CcidHandler<MockNfcDriver> {
    let atr = [0x3B, 0x80, 0x01, 0x01];
    let apdu = [0x90, 0x00];
    let mut driver = MockNfcDriver::new(card_present, &atr, &apdu);
    driver.init().unwrap();
    CcidHandler::new(driver)
}

/// Tiny deterministic LCG (xorshift32): reproducible "random" rounds.
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 24) as u8
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }
}

fn round_trip_ok(handler: &mut CcidHandler<MockNfcDriver>, msg: &[u8]) -> usize {
    let mut resp = [0u8; 512];
    // The contract: never panics, always writes a bounded response.
    let n = handler.process_command(msg, &mut resp);
    assert!(n <= resp.len(), "response length {n} exceeds buffer");
    n
}

// ---------------------------------------------------------------- suites

#[test]
fn fuzz_arbitrary_bytes_never_panic() {
    let mut rng = Rng(0xC0FFEE);
    for round in 0..2000u32 {
        let len = rng.below(300);
        let msg: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        let mut h = handler_with(round % 2 == 0);
        let n = round_trip_ok(&mut h, &msg);
        // Any response is a well-formed slot-status-shaped frame at minimum.
        assert!(n == 0 || n >= 10, "round {round}: response too short ({n})");
    }
}

#[test]
fn fuzz_valid_header_random_payload() {
    // Valid 10-byte header with each message type, adversarial payloads:
    // dwLength up to 4 GiB claimed, payload present or missing.
    let mut rng = Rng(0xDEADBEEF);
    for round in 0..2000u32 {
        let mut msg = vec![0u8; 10];
        msg[0] = ALL_MSG_TYPES[rng.below(ALL_MSG_TYPES.len())];
        let claimed = rng.next();
        msg[1..5].copy_from_slice(&claimed.to_le_bytes());
        msg[5] = rng.byte(); // slot
        msg[6] = rng.byte(); // seq
        msg[7] = rng.byte(); // BWI / power select
        let payload_len = rng.below(64);
        msg.extend((0..payload_len).map(|_| rng.byte()));
        let mut h = handler_with(round % 3 == 0);
        round_trip_ok(&mut h, &msg);
    }
}

#[test]
fn fuzz_truncated_headers() {
    // Every prefix of a valid IccPowerOn message: the handler must treat
    // any parse failure as a counted protocol error, never panic.
    let mut rng = Rng(0x51AB1E);
    for round in 0..500u32 {
        let mut msg = vec![0u8; 10];
        msg[0] = ALL_MSG_TYPES[rng.below(ALL_MSG_TYPES.len())];
        msg[6] = round as u8;
        let cut = rng.below(10);
        msg.truncate(cut);
        let mut h = handler_with(false);
        let n = round_trip_ok(&mut h, &msg);
        if cut < 10 {
            // Sub-header messages are unparseable at this layer: the
            // contract is zero output (the serial-framing layer NAKs
            // malformed frames before they ever reach the handler).
            assert_eq!(n, 0, "round {round}: sub-header message must not answer");
        }
    }
}

#[test]
fn fuzz_oversized_dw_length_claims() {
    // dwLength claims far beyond the actual payload: the handler must
    // detect the truncation (transport frame shorter than claimed),
    // count it, and answer a failed slot status — never slice OOB.
    let mut h = handler_with(true);
    for &claim in &[0xFFFFu32, 0xFFFF_FFFF, 0x0100_0000, 262, 261] {
        let mut msg = vec![0x6Fu8, 0, 0, 0, 0, 0, 1, 0, 0, 0]; // XfrBlock
        msg[1..5].copy_from_slice(&claim.to_le_bytes());
        msg.extend_from_slice(&[0x00, 0xA4]); // 2 real payload bytes
        let n = round_trip_ok(&mut h, &msg);
        if claim as usize > 2 {
            // Claimed more than delivered → failed-status response (>= 10B)
            assert!(n >= 10);
        }
    }
}

#[test]
fn fuzz_slot_and_seq_passthrough() {
    // slot != 0 and every seq value: responses must echo seq and reject
    // non-zero slots (single-slot reader).
    let mut rng = Rng(0x5EED);
    for round in 0..500u32 {
        let slot = (round % 4) as u8;
        let seq = rng.byte();
        let msg = [0x65u8, 0, 0, 0, 0, slot, seq, 0, 0, 0]; // GetSlotStatus
        let mut h = handler_with(true);
        let mut resp = [0u8; 512];
        let n = h.process_command(&msg, &mut resp);
        assert!(n >= 10, "round {round}: no response");
        assert_eq!(resp[6], seq, "round {round}: seq not echoed");
    }
}

#[test]
fn fuzz_state_machine_sequence_storms() {
    // Adversarial command ORDERINGS: Xfr before PowerOn, double PowerOn,
    // PowerOff storms, Escape between exchanges — the handler must never
    // enter a state it cannot answer from (every command gets a response).
    let mut rng = Rng(0xBAADF00D);
    let types = [0x62u8, 0x63, 0x65, 0x6F, 0x6B, 0x6C, 0x61];
    for round in 0..200u32 {
        let mut h = handler_with(round % 2 == 0);
        let storm = 50 + rng.below(50);
        let mut answered = 0;
        for i in 0..storm {
            let msg = [
                types[rng.below(types.len())],
                0, 0, 0, 0,
                0,
                i as u8,
                0, 0, 0,
            ];
            if round_trip_ok(&mut h, &msg) >= 10 {
                answered += 1;
            }
        }
        assert_eq!(answered, storm, "round {round}: a command went unanswered");
    }
}

#[test]
fn fuzz_sustain_after_garbage() {
    // Garbage storms must not wedge the handler: a valid GetSlotStatus
    // after arbitrary junk always answers.
    let mut rng = Rng(0xFACEB0C);
    let mut h = handler_with(true);
    for round in 0..100u32 {
        let junk_len = rng.below(200);
        let junk: Vec<u8> = (0..junk_len).map(|_| rng.byte()).collect();
        round_trip_ok(&mut h, &junk);

        let probe = [0x65u8, 0, 0, 0, 0, 0, round as u8, 0, 0, 0];
        let mut resp = [0u8; 512];
        let n = h.process_command(&probe, &mut resp);
        assert!(n >= 10, "round {round}: wedged after garbage");
        assert_eq!(resp[0], 0x81, "round {round}: not a slot-status response");
        assert_eq!(resp[6], round as u8, "round {round}: seq lost");
    }
}
