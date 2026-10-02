#![no_main]

use libfuzzer_sys::fuzz_target;
use pn7160_nci::{mock::MockTransport, Frame, MT_CMD, MT_DATA, MT_NTF, MT_RSP};

// Fuzz the pn7160-nci NCI frame decoder and bring-up ladder with arbitrary
// bytes, mirroring the random-input tests in `crates/pn7160-nci/src/lib.rs`
// (`hammer_decode_no_panic`, `hammer_ladder_terminates_no_panic`):
// `Frame::decode`, `is_rsp_to` and `run_ladder` are total over arbitrary
// input — any panic or hang is a finding. decode is one bounded pass over a
// three-octet header; the ladder sends a fixed five-command list, so both
// are structurally terminating.
fuzz_target!(|data: &[u8]| {
    if let Some(f) = Frame::decode(data) {
        // Structural invariants of any accepted frame (NCI §3.3).
        assert!(data.len() >= pn7160_nci::HEADER_LEN + f.len);
        assert_eq!(f.len, data[2] as usize, "payload length disagrees with LEN octet");
        assert!(f.len <= 255, "payload length exceeds one-octet LEN maximum");
        assert!(
            f.mt == MT_DATA || f.mt == MT_CMD || f.mt == MT_RSP || f.mt == MT_NTF,
            "message-type bits outside the four defined values"
        );
        assert_eq!(f.gid, data[0] & 0x0F, "GID disagrees with octet 0");
        assert_eq!(f.oid, data[1], "OID disagrees with octet 1");
        assert_eq!(&f.payload[..f.len], &data[3..3 + f.len], "payload copy disagrees");

        // is_rsp_to must agree with its definition against arbitrary commands.
        let cmd: &[u8] = if data.len() >= 2 { &data[..2] } else { data };
        assert_eq!(
            f.is_rsp_to(cmd),
            cmd.len() >= 2 && f.mt == MT_RSP && f.gid == (cmd[0] & 0x0F) && f.oid == cmd[1],
            "is_rsp_to disagrees with its GID/OID definition"
        );
    }

    // Scripted-ladder robustness: replies carved from the first half of the
    // input, notifications from the second. The ladder may legitimately fail
    // (Err) on garbage; it must never panic, hang, or exceed its command list.
    let mut t = MockTransport::new();
    let (a, b) = data.split_at(data.len() / 2);
    for chunk in a.chunks(8) {
        if chunk.len() >= pn7160_nci::HEADER_LEN {
            t.push_reply(chunk);
        }
    }
    for chunk in b.chunks(8) {
        if chunk.len() >= pn7160_nci::HEADER_LEN {
            t.push_notification(chunk);
        }
    }
    let _ = pn7160_nci::run_ladder(&mut t);
    assert!(t.sent.len() <= 5, "ladder sent more than its five commands");
});
