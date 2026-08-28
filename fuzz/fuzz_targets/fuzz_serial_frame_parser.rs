#![no_main]

use ccid_transport_serial::{calculate_lrc, FrameEvent, FrameParser, CTRL_ACK, SYNC};
use libfuzzer_sys::fuzz_target;

/// Bound the per-input byte budget. Well-formed frames cap at
/// 2 + 271 + 1 = 274 bytes (SYNC, CTRL, CCID message, LRC), so this is a
/// generous bound whose only purpose is to make the budget explicit.
const MAX_INPUT_BYTES: usize = 4096;

// Feed an arbitrary byte stream into the GemPC Twin serial frame parser,
// mirroring the parser semantics tested in
// `crates/ccid-transport-serial/src/lib.rs`: any panic or hang is a finding,
// and a frame is only ever accepted as a command when its LRC byte is
// exactly the XOR of every preceding frame byte.
fuzz_target!(|data: &[u8]| {
    let mut parser = FrameParser::new();

    for &byte in data.iter().take(MAX_INPUT_BYTES) {
        if let Some(event) = parser.feed(byte) {
            match event {
                FrameEvent::Command { ccid_bytes } => {
                    let frame = parser.received_frame_bytes();
                    let n = ccid_bytes.len();

                    // Structural invariant of an accepted frame:
                    // SYNC, CTRL_ACK, the CCID message, then the LRC byte.
                    assert_eq!(frame.len(), 2 + n + 1, "accepted malformed frame");
                    assert_eq!(frame[0], SYNC);
                    assert_eq!(frame[1], CTRL_ACK);
                    assert_eq!(&frame[2..2 + n], &ccid_bytes[..]);

                    // Never accept a frame with an invalid LRC.
                    assert_eq!(
                        frame[frame.len() - 1],
                        calculate_lrc(&frame[..frame.len() - 1]),
                        "parser accepted a frame with an invalid LRC"
                    );
                }
                FrameEvent::Error(_) => {}
            }
        }
    }
});
