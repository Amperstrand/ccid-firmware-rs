//! no_std NCI protocol core for the PN7160 NFC controller.
//!
//! Reader-mode bring-up ladder and wire framing, ported from the proven C
//! driver in the nucula wallet firmware (`components/pn7160/nci.c`) and
//! annotated against the NCI 2.0 specification and NXP PN7160 documentation.
//! The PN7160 implements NCI 2.0 (UM11495 §3.4).
//!
//! Spec citation convention (mirrors the repo's CCID_SPEC/CCID_SERIAL/GEMPC
//! markers; see specquotes.toml): comments cite NCI spec section numbers and
//! UM11495 sections. A tracked text source for greatspectations enforcement
//! needs an owner decision on the reference submodule — see issue #62.
//!
//! Layering:
//! - [`Frame`]: pure header decode; [`tx`] byte-for-byte command encoders.
//! - [`Transport`] + [`run_ladder`]: bring-up as testable orchestration over
//!   any transport (mock in tests, I2C in firmware). Mirrors the C sequence
//!   exactly, including the CORE_RESET notification drain and the stale-NTF
//!   flush before CORE_INIT (nci.c:197-207, 218-226).

#![cfg_attr(not(feature = "std"), no_std)]

use heapless::Vec;

/// NCI control packet header is exactly 3 octets — NCI 2.0 §3.3:
/// octet0 = MT[7:5] | PBF[4] | GID[3:0]; octet1 = OID[7:0]; octet2 = LEN.
/// (Ground truth: the C driver sends CORE_RESET as 20 00 01 01 — GID 0 in
/// octet0, full OID 0x00 as octet1 — and matches responses the same way.)
pub const HEADER_LEN: usize = 3;

/// Maximum NCI frame: 3 header octets + 255 payload (LEN is one octet,
/// NCI §3.3). Mirrors NCI_MAX_FRAME_SIZE in the nucula C driver's nci.h.
pub const MAX_FRAME: usize = HEADER_LEN + 255;

// --- Message types: MT bits 7..5 of octet 0 (NCI §3.3, Table 4) ----------
pub const MT_DATA: u8 = 0x00;
pub const MT_CMD: u8 = 0x20;
pub const MT_RSP: u8 = 0x40;
pub const MT_NTF: u8 = 0x60;

// --- Group IDs (octet 0 low nibble; NCI §5.1 CORE=0x00, §6.1 RF=0x01) ----
pub const GID_CORE: u8 = 0x00;
pub const GID_RF: u8 = 0x01;

// --- Core OIDs (octet 1; NCI §5.1) ----------------------------------------
pub const OID_CORE_RESET: u8 = 0x00;
pub const OID_CORE_INIT: u8 = 0x01;
pub const OID_CORE_SET_CONFIG: u8 = 0x02;

// --- RF OIDs (NCI §6.1) ---------------------------------------------------
pub const OID_RF_DISCOVER_MAP: u8 = 0x00;
pub const OID_RF_DISCOVER: u8 = 0x03;
pub const OID_RF_DISCOVER_SELECT: u8 = 0x04;

// --- RF technologies / protocols / interfaces (NCI §6, Tables 70-72) ------
pub const RF_TECH_PASSIVE_NFCA: u8 = 0x00;
pub const NCI_PROTOCOL_ISO_DEP: u8 = 0x04; // ISO-DEP (ISO/IEC 14443-4)
pub const NCI_INTERFACE_ISO_DEP: u8 = 0x02;

/// Status code STATUS_OK (NCI §5.1.5, Table 67).
pub const STATUS_OK: u8 = 0x00;

/// One decoded NCI frame. GID and OID are kept separate, exactly as they
/// sit on the wire (GID in octet0, OID as full octet1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub mt: u8,
    pub gid: u8,
    pub oid: u8,
    pub payload: [u8; 255],
    pub len: usize,
}

impl Frame {
    /// Decode a full frame from wire bytes. Returns `None` on short buffer,
    /// or when PBF is set (fragmented packets never occur for <=258B frames
    /// on this link; NCI §3.3 reassembly is out of scope).
    ///
    /// On the wire, one NCI packet arrives as one I2C read sequence per
    /// UM11495 §4.2 (the C driver's nci_read: 3-byte header transaction,
    /// then a LEN-byte payload transaction).
    pub fn decode(buf: &[u8]) -> Option<Frame> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let mt = buf[0] & 0xE0;
        let gid = buf[0] & 0x0F;
        if (buf[1] & 0x08) != 0 {
            return None; // PBF set: fragmented, unsupported
        }
        let oid = buf[1];
        let plen = buf[2] as usize;
        if buf.len() < HEADER_LEN + plen {
            return None;
        }
        let mut payload = [0u8; 255];
        payload[..plen].copy_from_slice(&buf[HEADER_LEN..HEADER_LEN + plen]);
        Some(Frame { mt, gid, oid, payload, len: plen })
    }

    /// Is this frame the RESPONSE to the given command bytes?
    /// Matches on MT=RSP, GID and OID equality.
    pub fn is_rsp_to(&self, cmd: &[u8]) -> bool {
        cmd.len() >= 2
            && self.mt == MT_RSP
            && self.gid == (cmd[0] & 0x0F)
            && self.oid == cmd[1]
    }

    /// Payload status octet (first payload byte) for RSPs, if present.
    /// CORE_INIT_RSP / SET_CONFIG_RSP / RF_*_RSP all lead with status.
    pub fn status(&self) -> Option<u8> {
        if self.len >= 1 { Some(self.payload[0]) } else { None }
    }
}

/// Encoded TX frames — byte-for-byte the sequences the proven C driver sends.
pub mod tx {
    use super::*;

    /// CORE_RESET with reset_type=KEEP_CONFIGURATION (NCI §5.1.1 payload
    /// [reset_type]; 0x01 = keep configuration) — nci.c:187.
    pub const CORE_RESET: [u8; 4] = [MT_CMD, OID_CORE_RESET, 0x01, 0x01];

    /// CORE_INIT (NCI 2.0 §5.1.2: two parameter octets) — nci.c:214.
    pub const CORE_INIT: [u8; 5] = [MT_CMD, OID_CORE_INIT, 0x02, 0x00, 0x00];

    /// CORE_SET_CONFIG setting TC1 = 0x00 (plain ISO-DEP: no DID, no NAD).
    /// Payload [num_params=1, id=0x52(TC1), len=1, value=0] — NCI §5.1.3 and
    /// Annex F (TC1 definition); nci.c:240.
    pub const SET_CONFIG_TC1: [u8; 7] =
        [MT_CMD, OID_CORE_SET_CONFIG, 0x04, 0x01, 0x52, 0x01, 0x00];

    /// RF_DISCOVER_MAP with one mapping: ISO-DEP protocol to ISO-DEP
    /// interface, poll mode. Payload [num=1, protocol, interface, mode=1]
    /// — NCI §6.1.1 (mapping mode 1 = poll) — reader-flavoured analogue of
    /// the C driver's card-emulation mapping (nci_configure_cardemu_mode).
    pub const RF_DISCOVER_MAP_ISO_DEP: [u8; 7] = [
        MT_CMD | GID_RF,
        OID_RF_DISCOVER_MAP,
        0x04,
        0x01,
        NCI_PROTOCOL_ISO_DEP,
        NCI_INTERFACE_ISO_DEP,
        0x01,
    ];

    /// RF_DISCOVER with one poll entry: passive NFC-A at 106 kb/s.
    /// Payload [num_entries=1, tech_and_mode, frequency, duration=0] —
    /// NCI §6.2.1 Table 80 (A0 = passive NFC-A poll) and Table 81 (00 =
    /// 106 kb/s); duration 0 = until another RF command.
    pub const RF_DISCOVER_PASSIVE_A: [u8; 7] = [
        MT_CMD | GID_RF,
        OID_RF_DISCOVER,
        0x04,
        0x01,
        RF_TECH_PASSIVE_NFCA,
        0x00,
        0x00,
    ];
}

/// A link-level NCI transport: send a command, get its response; drain a
/// pending notification. Implemented by I2C in firmware and by the mock in
/// tests. Mirrors the C driver's nci_transceive / nci_read split.
pub trait Transport {
    /// Send `cmd`, wait for the reply, return the decoded RSP frame.
    fn transact(&mut self, cmd: &[u8]) -> Option<Frame>;
    /// Read one already-pending notification (no command sent).
    fn drain(&mut self) -> Option<Frame>;
}

/// Bring-up steps, in C-driver order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    CoreReset,
    CoreInit,
    SetConfigTc1,
    RfDiscoverMap,
    RfDiscover,
}

impl Step {
    pub fn tx_bytes(self) -> &'static [u8] {
        match self {
            Step::CoreReset => &tx::CORE_RESET,
            Step::CoreInit => &tx::CORE_INIT,
            Step::SetConfigTc1 => &tx::SET_CONFIG_TC1,
            Step::RfDiscoverMap => &tx::RF_DISCOVER_MAP_ISO_DEP,
            Step::RfDiscover => &tx::RF_DISCOVER_PASSIVE_A,
        }
    }
}

/// Run the full reader-mode bring-up ladder over `t`, mirroring the C
/// sequence exactly:
///  1. CORE_RESET — expect RSP, then the CORE_RESET_NTF that NCI 2.0 sends
///     after it (nci.c:197-207 consumes it; anti-tearing GENERIC_ERROR_NTF
///     is tolerated the same way).
///  2. Flush up to three stale notifications before CORE_INIT (nci.c:218).
///  3. CORE_INIT — RSP status must be OK (nci.c:214-232).
///  4. CORE_SET_CONFIG(TC1=0) — status OK (nci.c:240).
///  5. RF_DISCOVER_MAP(ISO-DEP) and RF_DISCOVER(passive A) — status OK.
pub fn run_ladder<T: Transport>(t: &mut T) -> Result<(), &'static str> {
    // 1. CORE_RESET + notification drain.
    let rsp = t.transact(&tx::CORE_RESET).ok_or("no CORE_RESET response")?;
    if !rsp.is_rsp_to(&tx::CORE_RESET) {
        return Err("malformed CORE_RESET response");
    }
    let _ = t.drain(); // CORE_RESET_NTF (NCI 2.0 always follows; tolerate absence)

    // 2. Flush stale notifications before INIT (C driver allows up to 3).
    for _ in 0..3 {
        if t.drain().is_none() {
            break;
        }
    }

    // 3-5. Straight command/response ladder with status checks.
    for step in [Step::CoreInit, Step::SetConfigTc1, Step::RfDiscoverMap, Step::RfDiscover] {
        let rsp = t.transact(step.tx_bytes()).ok_or("transport ran dry")?;
        if !rsp.is_rsp_to(step.tx_bytes()) {
            return Err("response does not match command");
        }
        if rsp.status() != Some(STATUS_OK) {
            return Err("step status != OK");
        }
    }
    Ok(())
}

/// Mock transport for unit tests, modeling the real link's semantics:
/// `transact` consumes from the scripted reply queue (one RSP per command,
/// in order); `drain` consumes from the notification queue (unsolicited,
/// IRQ-gated — an empty queue returns None, never a future RSP).
#[cfg(any(test, feature = "std"))]
pub mod mock {
    use super::*;
    use std::collections::VecDeque;
    use std::vec::Vec as StdVec;

    pub struct MockTransport {
        pub sent: StdVec<StdVec<u8>>,
        pub replies: VecDeque<StdVec<u8>>,
        pub notifications: VecDeque<StdVec<u8>>,
        pub drained: usize,
    }

    impl MockTransport {
        pub fn new() -> Self {
            MockTransport {
                sent: StdVec::new(),
                replies: VecDeque::new(),
                notifications: VecDeque::new(),
                drained: 0,
            }
        }

        /// Script a command response (consumed by the matching `transact`).
        pub fn push_reply(&mut self, raw: &[u8]) {
            self.replies.push_back(raw.to_vec());
        }

        /// Script an unsolicited notification (consumed by `drain`).
        pub fn push_notification(&mut self, raw: &[u8]) {
            self.notifications.push_back(raw.to_vec());
        }
    }

    impl Transport for MockTransport {
        fn transact(&mut self, cmd: &[u8]) -> Option<Frame> {
            self.sent.push(cmd.to_vec());
            self.replies.pop_front().and_then(|raw| Frame::decode(&raw))
        }
        fn drain(&mut self) -> Option<Frame> {
            let f = self.notifications.pop_front().and_then(|raw| Frame::decode(&raw));
            if f.is_some() {
                self.drained += 1;
            }
            f
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rsp(octet0: u8, oid: u8, payload: &[u8]) -> std::vec::Vec<u8> {
        let mut v = std::vec::Vec::new();
        v.push(octet0);
        v.push(oid);
        v.push(payload.len() as u8);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn decode_header_and_payload() {
        let raw = [0x40, 0x03, 0x02, 0xAA, 0xBB]; // CORE RSP, OID 3, len 2
        let f = Frame::decode(&raw).unwrap();
        assert_eq!(f.mt, MT_RSP);
        assert_eq!(f.gid, GID_CORE);
        assert_eq!(f.oid, 0x03);
        assert_eq!(f.len, 2);
        assert_eq!(&f.payload[..2], &[0xAA, 0xBB]);
    }

    #[test]
    fn decode_rejects_short_and_fragmented() {
        assert!(Frame::decode(&[0x40, 0x00]).is_none());
        assert!(Frame::decode(&[0x40, 0x00, 0x02, 0x11]).is_none()); // short payload
        assert!(Frame::decode(&[0x40, 0x08, 0x00]).is_none()); // PBF set
    }

    #[test]
    fn tx_frames_match_c_driver_bytes() {
        assert_eq!(&tx::CORE_RESET, &[0x20, 0x00, 0x01, 0x01]);
        assert_eq!(&tx::CORE_INIT, &[0x20, 0x01, 0x02, 0x00, 0x00]);
        assert_eq!(&tx::SET_CONFIG_TC1, &[0x20, 0x02, 0x04, 0x01, 0x52, 0x01, 0x00]);
    }

    #[test]
    fn tx_rf_frames_shape() {
        // GID=RF(1) in octet0, full OID in octet1 (NCI §6.1).
        assert_eq!(tx::RF_DISCOVER_MAP_ISO_DEP[0], MT_CMD | GID_RF);
        assert_eq!(tx::RF_DISCOVER_MAP_ISO_DEP[1], OID_RF_DISCOVER_MAP);
        assert_eq!(tx::RF_DISCOVER_PASSIVE_A[1], OID_RF_DISCOVER);
        // NCI §6.2.1: entry = tech_and_mode A0 (passive A poll), freq 00 (106k)
        assert_eq!(tx::RF_DISCOVER_PASSIVE_A[4], RF_TECH_PASSIVE_NFCA);
        assert_eq!(tx::RF_DISCOVER_PASSIVE_A[5], 0x00);
    }

    #[test]
    fn is_rsp_to_matches_on_gid_and_oid() {
        // CORE_INIT cmd [20 01 ..] vs RSP octet0=0x40(GID 0), OID=0x01
        let ok = Frame::decode(&rsp(0x40, 0x01, &[0x00])).unwrap();
        assert!(ok.is_rsp_to(&tx::CORE_INIT));
        let wrong_oid = Frame::decode(&rsp(0x40, 0x02, &[0x00])).unwrap();
        assert!(!wrong_oid.is_rsp_to(&tx::CORE_INIT));
        let wrong_gid = Frame::decode(&rsp(0x41, 0x01, &[0x00])).unwrap();
        assert!(!wrong_gid.is_rsp_to(&tx::CORE_INIT));
        let ntf = Frame::decode(&rsp(0x60, 0x00, &[0x00])).unwrap();
        assert!(!ntf.is_rsp_to(&tx::CORE_RESET)); // NTF is not an RSP
    }

    #[test]
    fn ladder_happy_path_with_ntf_drain() {
        let mut t = mock::MockTransport::new();
        t.push_reply(&rsp(0x40, OID_CORE_RESET, &[0x00]));
        t.push_notification(&[0x60, OID_CORE_RESET, 0x01, 0x00]);
        t.push_reply(&rsp(0x40, OID_CORE_INIT, &[0x00]));
        t.push_reply(&rsp(0x40, OID_CORE_SET_CONFIG, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER_MAP, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER, &[0x00]));

        run_ladder(&mut t).expect("ladder should pass");
        assert_eq!(t.sent.len(), 5);
        assert_eq!(t.drained, 1);
    }

    #[test]
    fn ladder_flushes_stale_ntfs_before_init() {
        let mut t = mock::MockTransport::new();
        t.push_reply(&rsp(0x40, OID_CORE_RESET, &[0x00]));
        t.push_notification(&[0x60, OID_CORE_RESET, 0x01, 0x00]);
        t.push_notification(&[0x60, 0x07, 0x01, 0x00]);
        t.push_reply(&rsp(0x40, OID_CORE_INIT, &[0x00]));
        t.push_reply(&rsp(0x40, OID_CORE_SET_CONFIG, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER_MAP, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER, &[0x00]));

        run_ladder(&mut t).expect("stale NTFs are flushed, not judged");
        assert_eq!(t.sent.len(), 5);
        assert_eq!(t.drained, 2);
    }

    #[test]
    fn ladder_stops_on_bad_init_status() {
        let mut t = mock::MockTransport::new();
        t.push_reply(&rsp(0x40, OID_CORE_RESET, &[0x00]));
        t.push_notification(&[0x60, OID_CORE_RESET, 0x01, 0x00]);
        t.push_reply(&rsp(0x40, OID_CORE_INIT, &[0x02]));
        assert_eq!(run_ladder(&mut t), Err("step status != OK"));
    }

    #[test]
    fn ladder_stops_on_mismatched_response() {
        let mut t = mock::MockTransport::new();
        t.push_reply(&rsp(0x40, OID_CORE_RESET, &[0x00]));
        t.push_notification(&[0x60, OID_CORE_RESET, 0x01, 0x00]);
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER, &[0x00]));
        assert_eq!(run_ladder(&mut t), Err("response does not match command"));
    }

    #[test]
    fn ladder_stops_when_transport_runs_dry() {
        let mut t = mock::MockTransport::new();
        t.push_reply(&rsp(0x40, OID_CORE_RESET, &[0x00]));
        assert_eq!(run_ladder(&mut t), Err("transport ran dry"));
    }
}
