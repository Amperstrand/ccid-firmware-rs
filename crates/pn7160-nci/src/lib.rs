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

// --- RF OIDs only used by the reader session (NCI §6.1) -------------------
pub const OID_RF_DEACTIVATE: u8 = 0x06;

// --- Notification OIDs (octet 1 of an NTF; NCI §6.1) -----------------------
pub const NTF_RF_DISCOVER: u8 = 0x03;
pub const NTF_RF_INTF_ACTIVATED: u8 = 0x05;
pub const NTF_RF_DEACTIVATE: u8 = 0x06;
pub const OID_CORE_CONN_CREDITS: u8 = 0x06;

// --- DATA packet header bits (NCI §5.4.1; NXP nci_defs.h NCI_CID/PBF) ------
pub const CID_MASK: u8 = 0x0F;
pub const PBF_MASK: u8 = 0x10;

// --- Deactivation types (NCI §6.3.5.1, Table 98) ---------------------------
pub const DEACTIVATE_TYPE_IDLE: u8 = 0x00;
pub const DEACTIVATE_TYPE_SLEEP: u8 = 0x01;
pub const DEACTIVATE_TYPE_SLEEP_AF: u8 = 0x02;
pub const DEACTIVATE_TYPE_DISCOVERY: u8 = 0x03;

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
        if (buf[0] & PBF_MASK) != 0 {
            return None; // PBF set (octet 0 bit 4, NXP nci_defs.h): fragmented
        }
        let mt = buf[0] & 0xE0;
        let gid = buf[0] & 0x0F;
        let oid = buf[1];
        let plen = buf[2] as usize;
        if buf.len() < HEADER_LEN + plen {
            return None;
        }
        let mut payload = [0u8; 255];
        payload[..plen].copy_from_slice(&buf[HEADER_LEN..HEADER_LEN + plen]);
        Some(Frame {
            mt,
            gid,
            oid,
            payload,
            len: plen,
        })
    }

    /// Is this frame the RESPONSE to the given command bytes?
    /// Matches on MT=RSP, GID and OID equality.
    pub fn is_rsp_to(&self, cmd: &[u8]) -> bool {
        cmd.len() >= 2 && self.mt == MT_RSP && self.gid == (cmd[0] & 0x0F) && self.oid == cmd[1]
    }

    /// Payload status octet (first payload byte) for RSPs, if present.
    /// CORE_INIT_RSP / SET_CONFIG_RSP / RF_*_RSP all lead with status.
    pub fn status(&self) -> Option<u8> {
        if self.len >= 1 {
            Some(self.payload[0])
        } else {
            None
        }
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
    pub const SET_CONFIG_TC1: [u8; 7] = [MT_CMD, OID_CORE_SET_CONFIG, 0x04, 0x01, 0x52, 0x01, 0x00];

    /// CORE_SET_CONFIG TOTAL_DURATION (param id 0x0200) = 0x01FE — NCI
    /// §5.1.3 / Table 90. Without it the PN7160 completes ONE discovery
    /// cycle and stops emitting RF_DISCOVER_NTFs: the card is reported a
    /// single time, then presence flaps and activation starves (bench
    /// 2026-10-08). NOTE: the wallet's nci.c sends this MALFORMED (plen=5,
    /// param TLV missing the LEN octet) — the NFCC answers num_applied=0;
    /// harmless for its LISTEN mode, fatal for reader mode. Correct TLV:
    /// [num=1][id=0x0200 LE][len=2][value=0x01FE].
    pub const SET_CONFIG_TOTAL_DURATION: [u8; 9] = [
        MT_CMD,
        OID_CORE_SET_CONFIG,
        0x06,
        0x01,
        0x00,
        0x02,
        0x02,
        0xFE,
        0x01,
    ];

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
    /// Payload [num_entries=1, tech_and_mode, frequency] — the PN7160
    /// takes 2-byte entries with NO duration octet (the wallet's proven
    /// command is plen=3; sending plen=4 with a duration byte is
    /// rejected with status 0x05, bench 2026-10-08).
    pub const RF_DISCOVER_PASSIVE_A: [u8; 6] = [
        MT_CMD | GID_RF,
        OID_RF_DISCOVER,
        0x03,
        0x01,
        RF_TECH_PASSIVE_NFCA,
        0x01,
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

/// NCI DATA packet framing — NCI §5.4.1: octet 0 = PBF(bit 4) | CONN_ID(bits
/// 3:0), octet 1 = RFU (0), octet 2 = length. Byte-exact against the proven
/// C driver's `nci_send_data` (nucula nci.c), which uses CONN_ID 0.
pub mod driver;
pub mod transport;

pub mod data {
    use super::*;

    pub fn encode(conn_id: u8, payload: &[u8], out: &mut Vec<u8, MAX_FRAME>) -> bool {
        out.clear();
        if payload.len() > 255 {
            return false;
        }
        out.extend_from_slice(&[conn_id & CID_MASK, 0, payload.len() as u8])
            .is_ok()
            && out.extend_from_slice(payload).is_ok()
    }

    pub struct DataPacket {
        pub conn_id: u8,
        pub fragmented: bool,
        pub payload: [u8; 255],
        pub len: usize,
    }

    pub fn decode(buf: &[u8]) -> Option<DataPacket> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let plen = buf[2] as usize;
        if buf.len() < HEADER_LEN + plen {
            return None;
        }
        let mut payload = [0u8; 255];
        payload[..plen].copy_from_slice(&buf[HEADER_LEN..HEADER_LEN + plen]);
        Some(DataPacket {
            conn_id: buf[0] & CID_MASK,
            fragmented: (buf[0] & PBF_MASK) != 0,
            payload,
            len: plen,
        })
    }
}

/// Reader-mode session pieces: discovery notifications, tag selection,
/// DATA exchange, deactivation. Formats per NCI §6.3; DATA and deactivate
/// encodings are byte-exact against the nucula C driver.
pub mod reader {
    use super::*;

    /// RF_DEACTIVATE(IDLE) — byte-exact vs nci.c `nci_restart_discovery`
    /// stop[] = {NCI_MT_CMD | NCI_GID_RF, NCI_OID_RF_DEACTIVATE, 0x01, 0x00}.
    pub const RF_DEACTIVATE_IDLE: [u8; 4] = [
        MT_CMD | GID_RF,
        OID_RF_DEACTIVATE,
        0x01,
        DEACTIVATE_TYPE_IDLE,
    ];

    /// RF_DISCOVER_SELECT — NCI 2.0 §6.3.3.1 payload: [Discovery_ID,
    /// Protocol, Interface, Set_Params_Control(0x00 = defaults)]. NCI 1.x
    /// stacks use the 3-octet form; firmware binding validates on hardware.
    pub fn rf_discover_select(discovery_id: u8, protocol: u8, interface: u8) -> [u8; 7] {
        [
            MT_CMD | GID_RF,
            OID_RF_DISCOVER_SELECT,
            0x04,
            discovery_id,
            protocol,
            interface,
            0x00,
        ]
    }

    /// RF_DISCOVER_NTF fields the CardBackend needs — NCI 2.0 §6.3.2.3:
    /// payload = [Discovery_ID, Protocol, Tech_and_Mode, ParamsLen, Params..,
    /// Interface, (NCI 2.0: RF_Transmission_Technology)].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DiscoverNtf {
        pub discovery_id: u8,
        pub protocol: u8,
        pub tech_and_mode: u8,
        pub interface: u8,
        pub tech_params: [u8; 16],
        pub tech_params_len: usize,
    }

    impl DiscoverNtf {
        pub fn decode(f: &Frame) -> Option<DiscoverNtf> {
            if f.mt != MT_NTF || f.gid != GID_RF || f.oid != NTF_RF_DISCOVER || f.len < 5 {
                return None;
            }
            let params_len = f.payload[3] as usize;
            let interface_idx = 4 + params_len;
            if interface_idx >= f.len {
                return None;
            }
            let copy_len = params_len.min(16);
            let mut tech_params = [0u8; 16];
            tech_params[..copy_len].copy_from_slice(&f.payload[4..4 + copy_len]);
            Some(DiscoverNtf {
                discovery_id: f.payload[0],
                protocol: f.payload[1],
                tech_and_mode: f.payload[2],
                interface: f.payload[interface_idx],
                tech_params,
                tech_params_len: params_len,
            })
        }
    }

    /// NFC-A UID from DISCOVER_NTF tech params: [SENS_RES(2), NFCID1_LEN(1),
    /// NFCID1(N), SEL_RES] — NCI §6.3.2.3 / NCI Annex E.
    pub fn nfca_uid(ntf: &DiscoverNtf) -> Option<&[u8]> {
        if ntf.tech_params_len < 4 {
            return None;
        }
        let uid_len = ntf.tech_params[2] as usize;
        if ntf.tech_params_len < 3 + uid_len || uid_len == 0 {
            return None;
        }
        Some(&ntf.tech_params[3..3 + uid_len])
    }

    /// First fields of RF_INTF_ACTIVATED_NTF — NCI 2.0 §6.3.4: payload =
    /// [Discovery_ID, Interface, Protocol, Tech_and_Mode, Max_Data_Payload,
    /// Initial_Params...]. (ATS lives inside Initial_Params for ISO-DEP.)
    pub fn activated_summary(f: &Frame) -> Option<(u8, u8, u8)> {
        if f.mt != MT_NTF || f.gid != GID_RF || f.oid != NTF_RF_INTF_ACTIVATED || f.len < 3 {
            return None;
        }
        Some((f.payload[0], f.payload[1], f.payload[2]))
    }

    /// Drain notifications until a tag appears. CORE_CONN_CREDITS and other
    /// NTFs are skipped (the wallet's loop ignores credits the same way).
    pub fn wait_for_discovery<T: Transport>(t: &mut T) -> Option<DiscoverNtf> {
        for _ in 0..8 {
            let f = t.drain()?;
            if let Some(n) = DiscoverNtf::decode(&f) {
                return Some(n);
            }
        }
        None
    }

    /// Select the discovered tag; returns the INTF_ACTIVATED notification
    /// (its Initial_Params carry the ATS for ISO-DEP — NCI §6.3.4).
    pub fn select_tag<T: Transport>(t: &mut T, n: &DiscoverNtf) -> Result<Frame, &'static str> {
        let cmd = rf_discover_select(n.discovery_id, n.protocol, n.interface);
        let rsp = t.transact(&cmd).ok_or("no select response")?;
        if !rsp.is_rsp_to(&cmd) {
            return Err("select response mismatch");
        }
        if rsp.status() != Some(STATUS_OK) {
            return Err("select status != OK");
        }
        t.drain().ok_or("no activation notification")
    }

    /// Extract the ATS bytes from an RF_INTF_ACTIVATED notification's
    /// Initial_Params — NCI §6.3.4: payload = [ID, Interface, Protocol,
    /// Tech_and_Mode, Max_Data_Payload_Len, Initial_Params_Len, Params...].
    pub fn extract_ats(activation: &Frame) -> Option<&[u8]> {
        if activation.len < 6 {
            return None;
        }
        let params_len = activation.payload[5] as usize;
        if activation.len < 6 + params_len || params_len == 0 {
            return None;
        }
        Some(&activation.payload[6..6 + params_len])
    }

    /// Exchange one APDU over the RF data connection. The reply must be a
    /// DATA packet on the same connection (Frame::decode surfaces DATA
    /// packets with mt == MT_DATA and gid == conn_id).
    pub fn exchange<T: Transport>(t: &mut T, conn_id: u8, apdu: &[u8]) -> Option<Vec<u8, 255>> {
        let mut frame = Vec::new();
        if !data::encode(conn_id, apdu, &mut frame) {
            return None;
        }
        let rsp = t.transact(&frame)?;
        if rsp.mt != MT_DATA || rsp.gid != conn_id || rsp.len == 0 {
            return None;
        }
        Vec::from_slice(&rsp.payload[..rsp.len]).ok()
    }

    /// Deactivate to idle, then consume the DEACTIVATE notification —
    /// byte-exact semantics of nci.c `nci_restart_discovery`.
    pub fn deactivate_idle<T: Transport>(t: &mut T) -> Result<(), &'static str> {
        let rsp = t
            .transact(&RF_DEACTIVATE_IDLE)
            .ok_or("no deactivate response")?;
        if !rsp.is_rsp_to(&RF_DEACTIVATE_IDLE) {
            return Err("deactivate response mismatch");
        }
        t.drain().map(|_| ()).ok_or("no deactivate notification")
    }
}

/// Bring-up steps, in C-driver order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    CoreReset,
    CoreInit,
    SetConfigTc1,
    SetConfigTotalDuration,
    RfDiscoverMap,
    RfDiscover,
}

impl Step {
    pub fn tx_bytes(self) -> &'static [u8] {
        match self {
            Step::CoreReset => &tx::CORE_RESET,
            Step::CoreInit => &tx::CORE_INIT,
            Step::SetConfigTc1 => &tx::SET_CONFIG_TC1,
            Step::SetConfigTotalDuration => &tx::SET_CONFIG_TOTAL_DURATION,
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
    let rsp = t
        .transact(&tx::CORE_RESET)
        .ok_or("no CORE_RESET response")?;
    if !rsp.is_rsp_to(&tx::CORE_RESET) {
        return Err("malformed CORE_RESET response");
    }
    let _ = t.drain(); // CORE_RESET_NTF (NCI 2.0 always follows; tolerate absence)

    // 3-5. Straight command/response ladder with status checks.
    for step in [
        Step::CoreInit,
        Step::SetConfigTc1,
        Step::SetConfigTotalDuration,
        Step::RfDiscoverMap,
        Step::RfDiscover,
    ] {
        let rsp = t.transact(step.tx_bytes()).ok_or("transport ran dry")?;
        if !rsp.is_rsp_to(step.tx_bytes()) {
            return Err("response does not match command");
        }
        // SET_CONFIG_RSP payload is [num_params, status, ...] (NCI §5.1.3):
        // the status octet sits at index 1. Reading payload[0] compared the
        // PARAMETER COUNT against STATUS_OK — the real PN7160 answers
        // SET_CONFIG with num_params=1 → the ladder aborted every time
        // after a fully successful CORE_INIT.
        let status = if matches!(step, Step::SetConfigTc1 | Step::SetConfigTotalDuration) {
            if rsp.len >= 2 {
                Some(rsp.payload[1])
            } else {
                None
            }
        } else {
            rsp.status()
        };
        if status != Some(STATUS_OK) {
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
            let f = self
                .notifications
                .pop_front()
                .and_then(|raw| Frame::decode(&raw));
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
        assert!(Frame::decode(&[0x50, 0x00, 0x00]).is_none()); // PBF set: octet 0 bit 4 (NXP nci_defs.h)
    }

    #[test]
    fn tx_frames_match_c_driver_bytes() {
        assert_eq!(&tx::CORE_RESET, &[0x20, 0x00, 0x01, 0x01]);
        assert_eq!(&tx::CORE_INIT, &[0x20, 0x01, 0x02, 0x00, 0x00]);
        assert_eq!(
            &tx::SET_CONFIG_TC1,
            &[0x20, 0x02, 0x04, 0x01, 0x52, 0x01, 0x00]
        );
    }

    #[test]
    fn tx_rf_frames_shape() {
        // GID=RF(1) in octet0, full OID in octet1 (NCI §6.1).
        assert_eq!(tx::RF_DISCOVER_MAP_ISO_DEP[0], MT_CMD | GID_RF);
        assert_eq!(tx::RF_DISCOVER_MAP_ISO_DEP[1], OID_RF_DISCOVER_MAP);
        assert_eq!(tx::RF_DISCOVER_PASSIVE_A[1], OID_RF_DISCOVER);
        // NCI §6.2.1: entry = tech_and_mode A0 (passive A poll), freq 00 (106k)
        assert_eq!(tx::RF_DISCOVER_PASSIVE_A[4], RF_TECH_PASSIVE_NFCA);
        assert_eq!(tx::RF_DISCOVER_PASSIVE_A[5], 0x01);
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
        t.push_reply(&rsp(0x40, OID_CORE_SET_CONFIG, &[0x01, 0x00]));
        t.push_reply(&rsp(0x40, OID_CORE_SET_CONFIG, &[0x01, 0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER_MAP, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER, &[0x00]));

        run_ladder(&mut t).expect("ladder should pass");
        assert_eq!(t.sent.len(), 6);
        assert_eq!(t.drained, 1);
    }

    #[test]
    fn ladder_tolerates_reset_ntf_only() {
        let mut t = mock::MockTransport::new();
        t.push_reply(&rsp(0x40, OID_CORE_RESET, &[0x00]));
        t.push_notification(&[0x60, OID_CORE_RESET, 0x01, 0x00]);
        t.push_reply(&rsp(0x40, OID_CORE_INIT, &[0x00]));
        t.push_reply(&rsp(0x40, OID_CORE_SET_CONFIG, &[0x01, 0x00]));
        t.push_reply(&rsp(0x40, OID_CORE_SET_CONFIG, &[0x01, 0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER_MAP, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER, &[0x00]));

        run_ladder(&mut t).expect("ladder should pass with reset NTF");
        assert_eq!(t.sent.len(), 6);
        assert_eq!(t.drained, 1);
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

    #[test]
    fn hammer_decode_no_panic() {
        // Mirrors fuzz_targets/fuzz_nci_frame_decode.rs: decode and
        // is_rsp_to are total over arbitrary input.
        let mut s: u32 = 0x9E37_79B9;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        let mut buf = [0u8; 8];
        for _ in 0..10_000 {
            for b in buf.iter_mut() {
                *b = (next() >> 24) as u8;
            }
            let take = (next() as usize) % (buf.len() + 1);
            let input = &buf[..take];
            if let Some(f) = Frame::decode(input) {
                assert!(input.len() >= HEADER_LEN + f.len);
                assert_eq!(f.len, input[2] as usize);
                let _ = f.is_rsp_to(&input[..take.min(2)]);
            }
        }
    }

    #[test]
    fn hammer_ladder_terminates_no_panic() {
        // Mirrors the scripted-ladder half of fuzz_nci_frame_decode.rs: the
        // ladder fails cleanly (Err) on garbage replies; it must never panic
        // or exceed its five-command list.
        let mut s: u32 = 0xDEAD_BEEF;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        for _ in 0..200 {
            let mut t = mock::MockTransport::new();
            for _ in 0..8 {
                let frame = [
                    (next() >> 24) as u8,
                    (next() >> 24) as u8,
                    0x00,
                    (next() >> 24) as u8,
                ];
                t.push_reply(&frame);
            }
            let _ = run_ladder(&mut t);
            assert!(t.sent.len() <= 5);
        }
    }

    fn ntf_frame(oid: u8, payload: &[u8]) -> Frame {
        let mut p = [0u8; 255];
        p[..payload.len()].copy_from_slice(payload);
        Frame {
            mt: MT_NTF,
            gid: GID_RF,
            oid,
            payload: p,
            len: payload.len(),
        }
    }

    #[test]
    fn data_encode_matches_c_driver_bytes() {
        // nci.c nci_send_data: [MT_DATA(conn 0), RFU 0, len, payload]
        let mut out = Vec::new();
        assert!(data::encode(0, &[0xAA, 0xBB], &mut out));
        assert_eq!(out.as_slice(), &[0x00, 0x00, 0x02, 0xAA, 0xBB]);
    }

    #[test]
    fn data_round_trip() {
        let mut out = Vec::new();
        assert!(data::encode(3, &[1, 2, 3], &mut out));
        let pkt = data::decode(&out).unwrap();
        assert_eq!(pkt.conn_id, 3);
        assert!(!pkt.fragmented);
        assert_eq!(&pkt.payload[..pkt.len], &[1, 2, 3]);
    }

    #[test]
    fn data_decode_rejects_short() {
        assert!(data::decode(&[0x00, 0x00]).is_none());
        assert!(data::decode(&[0x00, 0x00, 0x02, 0x01]).is_none());
    }

    #[test]
    fn deactivate_idle_bytes_match_c_driver() {
        assert_eq!(&reader::RF_DEACTIVATE_IDLE, &[0x21, 0x06, 0x01, 0x00]);
    }

    #[test]
    fn discover_select_shape() {
        let cmd = reader::rf_discover_select(0x07, NCI_PROTOCOL_ISO_DEP, NCI_INTERFACE_ISO_DEP);
        assert_eq!(&cmd, &[0x21, 0x04, 0x04, 0x07, 0x04, 0x02, 0x00]);
    }

    #[test]
    fn discover_ntf_parse() {
        let f = ntf_frame(
            NTF_RF_DISCOVER,
            &[
                0x01,
                NCI_PROTOCOL_ISO_DEP,
                0x00,
                0x00,
                NCI_INTERFACE_ISO_DEP,
            ],
        );
        let n = reader::DiscoverNtf::decode(&f).unwrap();
        assert_eq!(n.discovery_id, 0x01);
        assert_eq!(n.protocol, NCI_PROTOCOL_ISO_DEP);
        assert_eq!(n.tech_and_mode, 0x00);
        assert_eq!(n.interface, NCI_INTERFACE_ISO_DEP);
    }

    #[test]
    fn discover_ntf_params_offset() {
        let f = ntf_frame(
            NTF_RF_DISCOVER,
            &[
                0x02,
                0x04,
                0x00,
                0x03,
                0x44,
                0x00,
                0x04,
                NCI_INTERFACE_ISO_DEP,
            ],
        );
        let n = reader::DiscoverNtf::decode(&f).unwrap();
        assert_eq!(n.discovery_id, 0x02);
        assert_eq!(n.interface, NCI_INTERFACE_ISO_DEP);
    }

    #[test]
    fn discover_ntf_rejects_malformed() {
        let overrun = ntf_frame(NTF_RF_DISCOVER, &[0x01, 0x04, 0x00, 0x05, 0x01]);
        assert!(reader::DiscoverNtf::decode(&overrun).is_none());
        let wrong = ntf_frame(NTF_RF_INTF_ACTIVATED, &[0x01, 0x04, 0x00, 0x00, 0x02]);
        assert!(reader::DiscoverNtf::decode(&wrong).is_none());
    }

    #[test]
    fn activated_summary_fields() {
        let f = ntf_frame(
            NTF_RF_INTF_ACTIVATED,
            &[0x01, NCI_INTERFACE_ISO_DEP, NCI_PROTOCOL_ISO_DEP],
        );
        assert_eq!(reader::activated_summary(&f), Some((0x01, 0x02, 0x04)));
    }

    #[test]
    fn reader_session_happy_path() {
        let mut t = mock::MockTransport::new();
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DISCOVER,
            0x05,
            0x01,
            NCI_PROTOCOL_ISO_DEP,
            0x00,
            0x00,
            NCI_INTERFACE_ISO_DEP,
        ]);
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_INTF_ACTIVATED,
            0x03,
            0x01,
            NCI_INTERFACE_ISO_DEP,
            NCI_PROTOCOL_ISO_DEP,
        ]);
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x01,
            DEACTIVATE_TYPE_IDLE,
        ]);
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DISCOVER_SELECT, 0x01, STATUS_OK]);
        t.push_reply(&[0x00, 0x00, 0x04, 0x90, 0x00, 0xAA, 0xBB]);
        t.push_reply(&[MT_RSP | GID_RF, OID_RF_DEACTIVATE, 0x01, STATUS_OK]);

        let n = reader::wait_for_discovery(&mut t).expect("tag should appear");
        reader::select_tag(&mut t, &n).expect("select ok");
        let rsp = reader::exchange(&mut t, 0, &[0x00, 0xA4, 0x04, 0x00]).expect("apdu reply");
        assert_eq!(rsp.as_slice(), &[0x90, 0x00, 0xAA, 0xBB]);
        reader::deactivate_idle(&mut t).expect("deactivate ok");
        assert_eq!(t.sent.len(), 3);
    }

    #[test]
    fn reader_exchange_fails_cleanly_when_link_lost() {
        let mut t = mock::MockTransport::new();
        assert!(reader::exchange(&mut t, 0, &[0x00]).is_none());
    }
}
