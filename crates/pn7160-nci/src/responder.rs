//! Deterministic synthetic-card responder — the #261 tier-a table
//! ported from `tests/hardware/nfc/vpicc_table.py` (byte-identical
//! doctrine, self-marking: every answer carries [`MARKER`] and/or
//! synthetic A5/5A filler so no RF trace can be mistaken for a real
//! SE). Serves the listen role only — never wallet funds, payment
//! credentials, or real JavaCard keys.

/// Visible synthetic identity for every FCI this table emits (the
/// vpicc_table marker convention, distinct string so a firmware listen
/// trace can never be attributed to the host-side VPICC-TABLE daemon).
pub const MARKER: &[u8] = b"PN7160-LISTEN";

/// The SELECT match key — the 7-byte GP ISD form `A0000001510000`
/// (vpicc_table.py:65; the fixture's bench-proven AID).
pub const ISD_AID: [u8; 7] = [0xA0, 0x00, 0x00, 0x00, 0x51, 0x00, 0x00];

/// The AID embedded in the FCI — the full 8-byte form
/// `A000000151000000` (vpicc_table.py:47).
const ISD_AID_FCI: [u8; 8] = [0xA0, 0x00, 0x00, 0x00, 0x51, 0x00, 0x00, 0x00];

type Buf = heapless::Vec<u8, 1024>;

fn tlv<const N: usize>(tag: u8, value: &[u8], out: &mut heapless::Vec<u8, N>) {
    // All values here are < 128 (same assertion as the fixture), so a
    // one-byte length is always sufficient.
    let _ = out.extend_from_slice(&[tag, value.len() as u8]);
    let _ = out.extend_from_slice(value);
}

fn fci(out: &mut Buf) {
    let mut inner: heapless::Vec<u8, 64> = heapless::Vec::new();
    tlv(0x84, &ISD_AID_FCI, &mut inner);
    tlv(0x79, b"\x60", &mut inner);
    tlv(0x90, MARKER, &mut inner);
    tlv(0x6F, &inner, out);
}

/// Answer one C-APDU from the fixed table. Clears `out`, then appends
/// the complete R-APDU including the status word.
pub fn answer(capdu: &[u8], out: &mut Buf) {
    out.clear();
    if capdu.len() < 4 {
        let _ = out.extend_from_slice(&[0x67, 0x00]);
        return;
    }
    match (capdu[0], capdu[1]) {
        (0x00, 0xA4) => {
            let lc = if capdu.len() > 4 {
                capdu[4] as usize
            } else {
                0
            };
            let end = (5 + lc).min(capdu.len());
            let aid = &capdu[5..end];
            if *aid == ISD_AID {
                fci(out);
                let _ = out.extend_from_slice(&[0x90, 0x00]);
            } else {
                let _ = out.extend_from_slice(&[0x6A, 0x82]);
            }
        }
        (0x80, 0x50) => {
            let _ = out.extend_from_slice(&[0x00, 0x00, 0x00]);
            let _ = out.extend_from_slice(&[0xA5; 8]);
            let _ = out.extend_from_slice(&[0x5A; 8]);
            let _ = out.extend_from_slice(&[0x90, 0x00]);
        }
        (0x84, 0x82) => {
            let _ = out.extend_from_slice(&[0x90, 0x00]);
        }
        (0x80, 0xCA) => {
            let _ = out.extend_from_slice(&[0x9F, 0x7F, 0x2D]);
            let _ = out.extend_from_slice(&[0xA5; 0x2D]);
            let _ = out.extend_from_slice(&[0x90, 0x00]);
        }
        (0x80, 0xF2) | (0x80, 0xE8) | (0x80, 0xE6) | (0x80, 0xE4) => {
            let _ = out.extend_from_slice(&[0x90, 0x00]);
        }
        _ => {
            let _ = out.extend_from_slice(&[0x6A, 0x82]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer_bytes(capdu: &[u8]) -> std::vec::Vec<u8> {
        let mut out = Buf::new();
        answer(capdu, &mut out);
        out.to_vec()
    }

    fn select(aid: &[u8]) -> std::vec::Vec<u8> {
        let mut v = std::vec::Vec::new();
        v.extend_from_slice(&[0x00, 0xA4, 0x04, 0x00, aid.len() as u8]);
        v.extend_from_slice(aid);
        v
    }

    #[test]
    fn isd_select_answers_self_marked_fci_and_9000() {
        let mut inner = std::vec::Vec::new();
        inner.extend_from_slice(&[0x84, ISD_AID_FCI.len() as u8]);
        inner.extend_from_slice(&ISD_AID_FCI);
        inner.extend_from_slice(&[0x79, 0x01, 0x60]);
        inner.extend_from_slice(&[0x90, MARKER.len() as u8]);
        inner.extend_from_slice(MARKER);
        let mut expect = std::vec::Vec::new();
        expect.extend_from_slice(&[0x6F, inner.len() as u8]);
        expect.extend_from_slice(&inner);
        expect.extend_from_slice(&[0x90, 0x00]);

        assert_eq!(answer_bytes(&select(&ISD_AID)), expect);
    }

    #[test]
    fn marker_is_visible_in_the_fci() {
        let rsp = answer_bytes(&select(&ISD_AID));
        let needle = MARKER;
        assert!(
            rsp.windows(needle.len()).any(|w| w == needle),
            "FCI must self-mark: {rsp:02X?}"
        );
    }

    #[test]
    fn unknown_aid_select_is_6a82() {
        assert_eq!(
            answer_bytes(&select(&[0xA0, 0x00, 0x00, 0x03, 0x33, 0x00, 0x01])),
            vec![0x6A, 0x82]
        );
    }

    #[test]
    fn eight_byte_isd_form_is_not_the_match_key() {
        // Faithful to the fixture: the 7-byte key matches, the 8-byte
        // FCI form does not (vpicc_table.py:65 vs :47).
        assert_eq!(answer_bytes(&select(&ISD_AID_FCI)), vec![0x6A, 0x82]);
    }

    #[test]
    fn initialize_update_answers_synthetic_filler() {
        let capdu = [0x80, 0x50, 0x00, 0x00, 0x08];
        let mut expect = std::vec::Vec::new();
        expect.extend_from_slice(&[0x00, 0x00, 0x00]);
        expect.extend_from_slice(&[0xA5; 8]);
        expect.extend_from_slice(&[0x5A; 8]);
        expect.extend_from_slice(&[0x90, 0x00]);
        assert_eq!(answer_bytes(&capdu), expect);
    }

    #[test]
    fn external_authenticate_acks() {
        assert_eq!(
            answer_bytes(&[0x84, 0x82, 0x00, 0x00, 0x10]),
            vec![0x90, 0x00]
        );
    }

    #[test]
    fn get_data_answers_cplc_shape() {
        let capdu = [0x80, 0xCA, 0x9F, 0x7F, 0x00];
        let mut expect = std::vec::Vec::new();
        expect.extend_from_slice(&[0x9F, 0x7F, 0x2D]);
        expect.extend_from_slice(&[0xA5; 0x2D]);
        expect.extend_from_slice(&[0x90, 0x00]);
        assert_eq!(answer_bytes(&capdu), expect);
    }

    #[test]
    fn gp_lifecycle_commands_ack() {
        for ins in [0xF2u8, 0xE8, 0xE6, 0xE4] {
            assert_eq!(
                answer_bytes(&[0x80, ins, 0x00, 0x00, 0x00]),
                vec![0x90, 0x00],
                "INS {ins:#04x}"
            );
        }
    }

    #[test]
    fn short_apdu_is_6700() {
        assert_eq!(answer_bytes(&[0x00, 0xA4, 0x04]), vec![0x67, 0x00]);
    }

    #[test]
    fn unknown_ins_is_6a82() {
        assert_eq!(
            answer_bytes(&[0x80, 0xCC, 0x00, 0x00, 0x00]),
            vec![0x6A, 0x82]
        );
    }
}
