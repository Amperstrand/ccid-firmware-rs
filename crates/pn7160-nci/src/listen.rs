//! ISO-DEP listen mode — card emulation by the DH-NFCEE (#261 RF rung).
//!
//! Scenario 1 of NXP AN13861 (rev 1.0, 2023-04-26, §2/§7.1) / UM11495
//! rev 1.8 §8.1.2 Table 81: the PN7160 is a pass-through; an external
//! reader activates us and its C-APDUs arrive as NCI DATA on the RF
//! logical connection. This module owns the listen bring-up ladder and
//! the session state machine; the answers themselves come from
//! [`crate::responder`] (synthetic, self-marked — never a real SE).
//!
//! Wire facts this module pins (authoritative sources; the historical
//! C wallet (zeugmaster/nucula, no license — facts only) agrees on
//! every byte but ignores credits and hardcodes conn 0, both of which
//! this implementation corrects):
//! - RF_DISCOVER_MAP mode octet 0x02 = listen (NCI §6.1.1); command
//!   `21 00 04 01 04 02 02`.
//! - CORE_SET_CONFIG listen params: LA_BIT_FRAME_SDD (0x30) = 0x04,
//!   LA_PLATFORM_CONFIG (0x31) = 0x00, LA_SEL_INFO (0x32) = 0x20 —
//!   AN13861 §5.1 Table 1 ("SENS_RES hardcoded to 0x0400 on Linux and
//!   Android; SEL_RES hardcoded to 0x20"), byte-identical to the first
//!   three parameters of AOSP's `SetConfigCommand` test vector
//!   (platform/system/nfc src/nci_packets.pdl:
//!   `\x20\x02\x10\x05\x30\x01\x04\x31\x01\x00\x32\x01\x20…`).
//! - RF_SET_LISTEN_MODE_ROUTING (kernel
//!   `NCI_OP_RF_SET_LISTEN_MODE_ROUTING_CMD` 0x2101 → wire `21 01`):
//!   ONE protocol-based routing entry [Qualifier 0x01, Length 0x03,
//!   Route 0x00 = DH-NFCEE, Power State 0x01, RF Protocol 0x04] —
//!   UM11495 §8.1.2 Table 81 ("PROTOCOL_ISO-DEP routed to DH-NFCEE").
//! - RF_DISCOVER listen entry tech-and-mode 0x80 = NFC_A_PASSIVE_
//!   LISTEN_MODE (UM11495 Table 28), command `21 03 03 01 80 01`.
//! - TOTAL_DURATION kept at 0x01FE (510 ms ≥ the 350 ms card-emulation
//!   minimum, UM11495 Table 14 remark).
//!
//! Session invariants (the two mission correctness rules):
//! - Connection ID is BOUND FROM THE WIRE (DATA octet-0 / credit
//!   notifications) and every later frame must agree, else the session
//!   tears down loudly. NCI's static RF connection is conn 0 (kernel
//!   `NCI_STATIC_RF_CONN_ID`) — the expected value, never an assumption.
//! - CORE_CONN_CREDITS_NTF is consumed and counted, never discarded:
//!   the PN7160 grants exactly ONE credit per logical connection
//!   (UM11495 §5.3.1 Table 10), so every DATA fragment we send consumes
//!   it and the next fragment gates on the re-grant
//!   ([`ListenSession::respond`]).

use super::data::{self, MAX_DATA_FRAGMENT};
use super::{
    is_conn_credits_ntf, is_wtx_ntf, Frame, Transport, GID_RF, MT_CMD, MT_DATA, MT_NTF,
    NCI_INTERFACE_ISO_DEP, NCI_PROTOCOL_ISO_DEP, NTF_RF_DEACTIVATE, NTF_RF_INTF_ACTIVATED,
    OID_CORE_SET_CONFIG, OID_RF_DISCOVER, OID_RF_DISCOVER_MAP, STATUS_OK,
};

/// RF group OID for RF_SET_LISTEN_MODE_ROUTING — kernel
/// `NCI_OP_RF_SET_LISTEN_MODE_ROUTING_CMD` = 0x2101 (octet0 `0x21`,
/// octet1 `0x01`); UM11495 Table 12: Full Support.
pub const OID_RF_SET_LISTEN_MODE_ROUTING: u8 = 0x01;

/// NFC-A passive LISTEN technology-and-mode — UM11495 Table 28
/// ("RF Technology and Mode: 0x80 (NFC_A_PASSIVE_LISTEN_MODE)").
pub const RF_TECH_PASSIVE_NFCA_LISTEN: u8 = 0x80;

/// RF_DISCOVER_MAP, one mapping: ISO-DEP protocol → ISO-DEP interface,
/// LISTEN mode (mode octet 0x02 — NCI §6.1.1; poll sibling carries 0x01).
pub const RF_DISCOVER_MAP_ISO_DEP_LISTEN: [u8; 7] = [
    MT_CMD | GID_RF,
    OID_RF_DISCOVER_MAP,
    0x04,
    0x01,
    NCI_PROTOCOL_ISO_DEP,
    NCI_INTERFACE_ISO_DEP,
    0x02,
];

/// CORE_SET_CONFIG with the three NFC-A listen parameters — AN13861
/// Table 1 + AOSP `nci_packets.pdl` listen vector (see module docs).
/// SENS_RES = 0x0400, SEL_RES = 0x20 (ISO-DEP T4T capable).
pub const SET_CONFIG_LISTEN_NFCA: [u8; 13] = [
    MT_CMD,
    OID_CORE_SET_CONFIG,
    0x0A,
    0x03, //
    0x30,
    0x01,
    0x04, // LA_BIT_FRAME_SDD
    0x31,
    0x01,
    0x00, // LA_PLATFORM_CONFIG
    0x32,
    0x01,
    0x20, // LA_SEL_INFO
];

/// RF_SET_LISTEN_MODE_ROUTING with one protocol-based entry routing
/// ISO-DEP to the DH (UM11495 §8.1.2 Table 81). Entry layout per
/// UM11495 Table 30's field order (Route before Power State before the
/// type-specific key).
pub const RF_SET_LISTEN_MODE_ROUTING_DH_ISO_DEP: [u8; 10] = [
    MT_CMD | GID_RF,
    OID_RF_SET_LISTEN_MODE_ROUTING,
    0x07, //
    0x00, // More: last message
    0x01, // one routing entry
    0x01, // qualifier: protocol-based
    0x03, // entry length
    0x00, // route: DH-NFCEE
    0x01, // power state: bit 0 (device on)
    NCI_PROTOCOL_ISO_DEP,
];

/// RF_DISCOVER with one LISTEN entry: NFC-A passive at 106 kb/s — the
/// same 2-byte-entry, no-duration-octet shape the poll ladder proved on
/// this silicon (sending plen=4 is rejected with 0x05, bench
/// 2026-10-08).
pub const RF_DISCOVER_PASSIVE_A_LISTEN: [u8; 6] = [
    MT_CMD | GID_RF,
    OID_RF_DISCOVER,
    0x03, //
    0x01,
    RF_TECH_PASSIVE_NFCA_LISTEN,
    0x01,
];

/// The listen bring-up ladder: CORE_RESET → CORE_INIT →
/// CORE_SET_CONFIG(TOTAL_DURATION) → CORE_SET_CONFIG(listen NFC-A) →
/// RF_DISCOVER_MAP(listen) → RF_SET_LISTEN_MODE_ROUTING →
/// RF_DISCOVER(listen) — UM11495 §8.1.2 Table 81 order, sharing the
/// reader ladder's CORE_RESET/INIT/TOTAL_DURATION prefix so both roles
/// bring the chip up identically.
pub fn run_ladder_listen<T: Transport>(t: &mut T) -> Result<(), &'static str> {
    let rsp = t
        .transact(&super::tx::CORE_RESET)
        .ok_or("no CORE_RESET response")?;
    if !rsp.is_rsp_to(&super::tx::CORE_RESET) {
        return Err("malformed CORE_RESET response");
    }
    let _ = t.drain(); // CORE_RESET_NTF follows in NCI 2.0; tolerate absence

    for step in [
        &super::tx::CORE_INIT[..],
        &super::tx::SET_CONFIG_TOTAL_DURATION[..],
        &SET_CONFIG_LISTEN_NFCA[..],
        &RF_DISCOVER_MAP_ISO_DEP_LISTEN[..],
        &RF_SET_LISTEN_MODE_ROUTING_DH_ISO_DEP[..],
        &RF_DISCOVER_PASSIVE_A_LISTEN[..],
    ] {
        let rsp = t.transact(step).ok_or("transport ran dry")?;
        if !rsp.is_rsp_to(step) {
            return Err("response does not match command");
        }
        if rsp.status() != Some(STATUS_OK) {
            return Err("listen ladder step rejected");
        }
        // Bench 2026-11-03 (PR #552 evidence): a reader polling during
        // the ladder sends activation NTFs that the transact stash
        // holds. Drain them here so the serve loop doesn't see stale
        // activations, and so the stash (4 slots, drop-oldest) doesn't
        // evict ladder-critical NTFs.
        while let Some(_f) = t.drain() {}
    }
    Ok(())
}

/// One MAX_DATA_PAYLOAD cap for responses: the NCI structural clamp
/// (253) AND the activation-advertised chip limit, whichever is
/// smaller — a chip advertising less than 253 must not be handed a
/// 253-octet fragment.
const fn fragment_cap(max_data_payload: u8) -> usize {
    let advertised = max_data_payload as usize;
    if advertised == 0 || advertised > MAX_DATA_FRAGMENT {
        MAX_DATA_FRAGMENT
    } else {
        advertised
    }
}

/// Why a response could not be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RespondError {
    #[error("no active listen session")]
    Inactive,
    #[error("connection id not yet bound by wire traffic")]
    ConnUnbound,
    /// A credit or DATA frame named a foreign connection id: the
    /// session was torn down (reset) — the main loop must re-arm.
    #[error("conn id divergence: bound {bound:#04x}, saw {seen:#04x}")]
    Divergence { bound: u8, seen: u8 },
    /// No CORE_CONN_CREDITS_NTF within the bounded wait: session torn
    /// down — the late grant must not land in the next poll window.
    #[error("credit re-grant did not arrive in budget")]
    CreditTimeout,
    #[error("DATA write failed on the link")]
    SendFailed,
}

/// Credit re-grant wait budget: 24 × 10 ms polls, mirroring the reader
/// exchange's strict-RSP budget (lib.rs `STRICT_RSP_POLLS`).
const CREDIT_POLLS: usize = 24;
const CREDIT_WAIT_MS: u64 = 10;

/// What one poll of the link produced for the listen role.
///
/// The Apdu variant carries the same inline 1 KiB heapless buffer the
/// reader path's `ExchangeOutcome` does (lib.rs) — identical stack
/// cost, no allocator to box it in (no_std).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenEvent {
    /// A reader activated us — NCI 2.0 RF_INTF_ACTIVATED_NTF
    /// ([DiscoveryID, Interface, Protocol, TechAndMode,
    /// MaxDataPayload, InitialNumCredits, …], kernel
    /// `nci_rf_intf_activated_ntf`). The conn id is NOT in this
    /// notification; it binds on the first DATA/credit frame.
    Activated {
        max_data_payload: u8,
        initial_credits: u8,
    },
    /// A complete reader C-APDU (PBF chain reassembled). The
    /// connection id is bound by the DATA packet(s) that carried it.
    Apdu(heapless::Vec<u8, 1024>),
    /// RF_DEACTIVATE_NTF — the reader left (or the NFCC tore the
    /// interface down). Reason byte as on the wire; the session is
    /// reset and discovery re-arms per NCI §5.2.5.
    ReaderLeft { reason: u8 },
    /// A DATA or credit frame named a different connection id than the
    /// wire-bound one — session reset; a violation of the static RF
    /// connection contract that must surface, not be absorbed.
    ConnDivergence { bound: u8, seen: u8 },
    /// The RX chain exceeded the 1 KiB reassembly buffer — session
    /// reset (a reader never legitimately exceeds it on this table).
    RxOverflow,
}

/// Listen-role session state: activation, wire-bound connection id,
/// credit counter, RX reassembly. One frame per [`poll`]; responses via
/// [`respond`](ListenSession::respond).
pub struct ListenSession {
    active: bool,
    conn_id: Option<u8>,
    credits: u8,
    max_data_payload: u8,
    rx: heapless::Vec<u8, 1024>,
}

impl Default for ListenSession {
    fn default() -> Self {
        Self::new()
    }
}

impl ListenSession {
    pub fn new() -> Self {
        ListenSession {
            active: false,
            conn_id: None,
            credits: 0,
            max_data_payload: 0,
            rx: heapless::Vec::new(),
        }
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// The wire-bound connection id (NCI static RF conn 0 is the
    /// expected value; this is what the wire actually said).
    pub fn conn_id(&self) -> Option<u8> {
        self.conn_id
    }

    /// Credits currently spendable on DH→NFCC DATA (PN7160: 0 or 1).
    pub fn credits(&self) -> u8 {
        self.credits
    }

    fn reset(&mut self) {
        self.active = false;
        self.conn_id = None;
        self.credits = 0;
        self.max_data_payload = 0;
        self.rx.clear();
    }

    /// Bind-or-verify a connection id; `Err` carries the divergence.
    fn bind_conn(&mut self, seen: u8) -> Result<(), (u8, u8)> {
        match self.conn_id {
            None => {
                self.conn_id = Some(seen);
                Ok(())
            }
            Some(bound) if bound == seen => Ok(()),
            Some(bound) => {
                self.reset();
                Err((bound, seen))
            }
        }
    }

    /// Apply a CORE_CONN_CREDITS_NTF payload: [num, {conn, credits}…]
    /// (NCI 1.0 §4.4.4 Table 102). Never discarded — every entry
    /// updates the counter for the bound connection.
    fn note_credits(&mut self, f: &Frame) -> Result<(), (u8, u8)> {
        if f.len < 1 {
            return Ok(());
        }
        let entries = (f.payload[0] as usize).min((f.len - 1) / 2);
        for i in 0..entries {
            let conn = f.payload[1 + i * 2];
            let granted = f.payload[2 + i * 2];
            self.bind_conn(conn)?;
            self.credits = self.credits.saturating_add(granted);
        }
        Ok(())
    }

    /// Poll ONE pending frame and classify it for the listen role.
    /// Returns `None` for consumed frames (WTX advisories, credit
    /// updates, RSPs) — keep polling; `Some(event)` carries state
    /// transitions. Consumes exactly one frame per call.
    pub fn poll<T: Transport>(&mut self, t: &mut T) -> Option<ListenEvent> {
        let f = t.poll_frame()?;
        if is_wtx_ntf(&f) {
            return None; // controller-owned advisory (UM11495 §7.3.4)
        }
        if is_conn_credits_ntf(&f) {
            if let Err((bound, seen)) = self.note_credits(&f) {
                return Some(ListenEvent::ConnDivergence { bound, seen });
            }
            return None;
        }
        if f.mt == MT_NTF && f.gid == GID_RF && f.oid == NTF_RF_INTF_ACTIVATED && f.len >= 6 {
            self.active = true;
            self.max_data_payload = f.payload[4];
            self.credits = f.payload[5];
            self.rx.clear();
            return Some(ListenEvent::Activated {
                max_data_payload: self.max_data_payload,
                initial_credits: self.credits,
            });
        }
        if f.mt == MT_NTF && f.gid == GID_RF && f.oid == NTF_RF_DEACTIVATE {
            let reason = if f.len >= 2 { f.payload[1] } else { 0 };
            self.reset();
            return Some(ListenEvent::ReaderLeft { reason });
        }
        if f.mt == MT_DATA {
            let conn = f.gid; // DATA octet-0 low nibble = conn id
            if let Err((bound, seen)) = self.bind_conn(conn) {
                return Some(ListenEvent::ConnDivergence { bound, seen });
            }
            if self.rx.extend_from_slice(&f.payload[..f.len]).is_err() {
                self.reset();
                return Some(ListenEvent::RxOverflow);
            }
            if !f.pbf {
                return Some(ListenEvent::Apdu(core::mem::replace(
                    &mut self.rx,
                    heapless::Vec::new(),
                )));
            }
            return None;
        }
        None // RSPs and unrelated NTFs: bounded-ignoral, exchange doctrine
    }

    /// Wait one credit-re-grant budget for credits on the bound conn.
    /// Consumes every frame that arrives while waiting (credits are
    /// applied; WTX consumed; a divergence tears the session down).
    fn await_credit<T: Transport>(&mut self, t: &mut T) -> Result<(), RespondError> {
        for _ in 0..CREDIT_POLLS {
            t.wait_ms(CREDIT_WAIT_MS);
            while let Some(f) = t.poll_frame() {
                if is_conn_credits_ntf(&f) {
                    if let Err((bound, seen)) = self.note_credits(&f) {
                        return Err(RespondError::Divergence { bound, seen });
                    }
                    if self.credits > 0 {
                        return Ok(());
                    }
                }
                // WTX advisories and anything else: consumed
            }
            if self.credits > 0 {
                return Ok(());
            }
        }
        self.reset();
        Err(RespondError::CreditTimeout)
    }

    /// Send a response as PBF-chained DATA fragments on the bound
    /// connection, credit-gated between fragments (PN7160: one credit
    /// per connection — UM11495 §5.3.1 Table 10). `Ok(())` when the
    /// last fragment was written to the link.
    pub fn respond<T: Transport>(&mut self, t: &mut T, rapdu: &[u8]) -> Result<(), RespondError> {
        if !self.active {
            return Err(RespondError::Inactive);
        }
        let conn = self.conn_id.ok_or(RespondError::ConnUnbound)?;
        let cap = fragment_cap(self.max_data_payload);
        let n = rapdu.len();
        if n == 0 {
            return Err(RespondError::SendFailed);
        }
        let mut out = heapless::Vec::new();
        for (i, chunk) in rapdu.chunks(cap).enumerate() {
            if self.credits == 0 {
                self.await_credit(t)?;
            }
            let pbf = (i + 1) * cap < n;
            if !data::encode_fragment(conn, pbf, chunk, &mut out) {
                self.reset();
                return Err(RespondError::SendFailed);
            }
            if !t.send(&out) {
                self.reset();
                return Err(RespondError::SendFailed);
            }
            self.credits -= 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockTransport;
    use crate::{tx, MT_RSP};

    fn ok_rsp(cmd: &[u8]) -> std::vec::Vec<u8> {
        std::vec![MT_RSP | (cmd[0] & 0x0F), cmd[1], 0x01, STATUS_OK]
    }

    // A listen-mode RF_INTF_ACTIVATED_NTF: [disc_id 0, interface
    // ISO-DEP, protocol ISO-DEP, tech-mode NFC-A listen 0x80,
    // max_data_payload 0xFF, initial_credits 1, tech_params_len 0].
    const ACTIVATION: [u8; 10] = [0x61, 0x05, 0x07, 0x00, 0x02, 0x04, 0x80, 0xFF, 0x01, 0x00];
    const CREDIT_CONN0: [u8; 6] = [0x60, 0x06, 0x03, 0x01, 0x00, 0x01];
    const CREDIT_CONN1: [u8; 6] = [0x60, 0x06, 0x03, 0x01, 0x01, 0x01];
    const WTX: [u8; 3] = [0x6F, 0x17, 0x00];
    const DEACTIVATE: [u8; 5] = [0x61, 0x06, 0x02, 0x00, 0x02];

    fn data_frame(conn_pbf: u8, payload: &[u8]) -> std::vec::Vec<u8> {
        let mut v = std::vec![conn_pbf, 0x00, payload.len() as u8];
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn discover_map_listen_bytes_are_pinned() {
        assert_eq!(
            RF_DISCOVER_MAP_ISO_DEP_LISTEN,
            [0x21, 0x00, 0x04, 0x01, 0x04, 0x02, 0x02]
        );
    }

    #[test]
    fn set_config_listen_nfca_bytes_are_pinned() {
        assert_eq!(
            SET_CONFIG_LISTEN_NFCA,
            [
                0x20, 0x02, 0x0A, 0x03, //
                0x30, 0x01, 0x04, 0x31, 0x01, 0x00, 0x32, 0x01, 0x20
            ]
        );
    }

    #[test]
    fn set_config_params_match_asosp_listen_vector() {
        // AOSP platform/system/nfc nci_packets.pdl SetConfigCommand
        // test vector: 20 02 10 05 30 01 04 31 01 00 32 01 20 38 01 01
        // 50 01 00 — our three NFC-A listen params must appear in it,
        // byte-for-byte, in order.
        let aosp: &[u8] = &[
            0x20, 0x02, 0x10, 0x05, //
            0x30, 0x01, 0x04, 0x31, 0x01, 0x00, 0x32, 0x01, 0x20, //
            0x38, 0x01, 0x01, 0x50, 0x01, 0x00,
        ];
        let ours = &SET_CONFIG_LISTEN_NFCA[4..];
        assert!(
            aosp.windows(ours.len()).any(|w| w == ours),
            "params {ours:02X?} must be a sub-sequence of the AOSP vector"
        );
    }

    #[test]
    fn routing_bytes_are_pinned() {
        assert_eq!(
            RF_SET_LISTEN_MODE_ROUTING_DH_ISO_DEP,
            [0x21, 0x01, 0x07, 0x00, 0x01, 0x01, 0x03, 0x00, 0x01, 0x04]
        );
    }

    #[test]
    fn discover_listen_bytes_are_pinned() {
        assert_eq!(
            RF_DISCOVER_PASSIVE_A_LISTEN,
            [0x21, 0x03, 0x03, 0x01, 0x80, 0x01]
        );
    }

    #[test]
    fn ladder_sends_seven_commands_in_authoritative_order() {
        let mut t = MockTransport::new();
        for cmd in [
            &tx::CORE_RESET[..],
            &tx::CORE_INIT[..],
            &tx::SET_CONFIG_TOTAL_DURATION[..],
            &SET_CONFIG_LISTEN_NFCA[..],
            &RF_DISCOVER_MAP_ISO_DEP_LISTEN[..],
            &RF_SET_LISTEN_MODE_ROUTING_DH_ISO_DEP[..],
            &RF_DISCOVER_PASSIVE_A_LISTEN[..],
        ] {
            t.push_reply(&ok_rsp(cmd));
        }
        assert_eq!(run_ladder_listen(&mut t), Ok(()));
        let sent: std::vec::Vec<_> = t.sent.iter().map(|s| s.as_slice()).collect();
        assert_eq!(
            sent,
            vec![
                &tx::CORE_RESET[..],
                &tx::CORE_INIT[..],
                &tx::SET_CONFIG_TOTAL_DURATION[..],
                &SET_CONFIG_LISTEN_NFCA[..],
                &RF_DISCOVER_MAP_ISO_DEP_LISTEN[..],
                &RF_SET_LISTEN_MODE_ROUTING_DH_ISO_DEP[..],
                &RF_DISCOVER_PASSIVE_A_LISTEN[..],
            ]
        );
    }

    #[test]
    fn ladder_aborts_when_listen_config_rejected() {
        let mut t = MockTransport::new();
        t.push_reply(&ok_rsp(&tx::CORE_RESET));
        t.push_reply(&ok_rsp(&tx::CORE_INIT));
        t.push_reply(&ok_rsp(&tx::SET_CONFIG_TOTAL_DURATION));
        t.push_reply(&[MT_RSP, OID_CORE_SET_CONFIG, 0x01, 0x02]);
        assert_eq!(
            run_ladder_listen(&mut t),
            Err("listen ladder step rejected")
        );
    }

    #[test]
    fn activation_binds_state_and_reports_event() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        let mut s = ListenSession::new();
        assert_eq!(
            s.poll(&mut t),
            Some(ListenEvent::Activated {
                max_data_payload: 0xFF,
                initial_credits: 1
            })
        );
        assert!(s.active());
        assert_eq!(s.credits(), 1);
    }

    #[test]
    fn single_data_packet_binds_conn_zero_and_yields_apdu() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        let apdu = [0x00, 0xA4, 0x04, 0x00, 0x02, 0x3B, 0x00];
        t.push_notification(&data_frame(0x00, &apdu));
        let mut s = ListenSession::new();
        let mut got = std::vec::Vec::new();
        for _ in 0..2 {
            if let Some(ListenEvent::Apdu(a)) = s.poll(&mut t) {
                got.extend_from_slice(&a);
            }
        }
        assert_eq!(got, apdu);
        assert_eq!(s.conn_id(), Some(0));
    }

    #[test]
    fn fragmented_data_reassembles_to_one_apdu() {
        let mut t = MockTransport::new();
        t.push_notification(&data_frame(0x10, &[0xAA; 200]));
        t.push_notification(&data_frame(0x00, &[0xBB; 60]));
        let mut s = ListenSession::new();
        let first = s.poll(&mut t);
        assert_eq!(first, None);
        match s.poll(&mut t) {
            Some(ListenEvent::Apdu(a)) => {
                assert_eq!(a.len(), 260);
                assert_eq!(&a[..200], &[0xAA; 200]);
                assert_eq!(&a[200..], &[0xBB; 60]);
            }
            other => panic!("expected Apdu, got {other:?}"),
        }
    }

    #[test]
    fn credits_notification_updates_counter() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        t.push_notification(&CREDIT_CONN0);
        let mut s = ListenSession::new();
        s.poll(&mut t);
        s.poll(&mut t);
        assert_eq!(s.credits(), 2);
        assert_eq!(s.conn_id(), Some(0));
    }

    #[test]
    fn foreign_conn_credit_tears_down() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        t.push_notification(&data_frame(0x00, &[0x00, 0xA4, 0x04, 0x00]));
        t.push_notification(&CREDIT_CONN1);
        let mut s = ListenSession::new();
        s.poll(&mut t);
        s.poll(&mut t);
        assert_eq!(
            s.poll(&mut t),
            Some(ListenEvent::ConnDivergence { bound: 0, seen: 1 })
        );
        assert!(!s.active());
        assert_eq!(s.conn_id(), None);
    }

    #[test]
    fn data_on_foreign_conn_tears_down() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        t.push_notification(&data_frame(0x00, &[0x00, 0xA4, 0x04, 0x00]));
        t.push_notification(&data_frame(0x01, &[0x00, 0xA4, 0x04, 0x00]));
        let mut s = ListenSession::new();
        s.poll(&mut t);
        s.poll(&mut t);
        assert_eq!(
            s.poll(&mut t),
            Some(ListenEvent::ConnDivergence { bound: 0, seen: 1 })
        );
        assert!(!s.active());
    }

    #[test]
    fn deactivate_ntf_resets_session() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        t.push_notification(&data_frame(0x00, &[0x00, 0xA4, 0x04, 0x00]));
        t.push_notification(&DEACTIVATE);
        let mut s = ListenSession::new();
        s.poll(&mut t);
        s.poll(&mut t);
        assert_eq!(s.poll(&mut t), Some(ListenEvent::ReaderLeft { reason: 2 }));
        assert!(!s.active());
        assert_eq!(s.conn_id(), None);
    }

    #[test]
    fn wtx_advisory_is_consumed() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        t.push_notification(&WTX);
        let mut s = ListenSession::new();
        s.poll(&mut t);
        assert_eq!(s.poll(&mut t), None);
        assert!(s.active());
    }

    #[test]
    fn respond_before_activation_is_inactive() {
        let mut t = MockTransport::new();
        let mut s = ListenSession::new();
        assert_eq!(
            s.respond(&mut t, &[0x90, 0x00]),
            Err(RespondError::Inactive)
        );
    }

    #[test]
    fn respond_before_any_data_is_conn_unbound() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        let mut s = ListenSession::new();
        s.poll(&mut t);
        assert_eq!(
            s.respond(&mut t, &[0x90, 0x00]),
            Err(RespondError::ConnUnbound)
        );
    }

    #[test]
    fn respond_single_fragment_sends_one_data_packet() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        t.push_notification(&data_frame(0x00, &[0x00, 0xA4, 0x04, 0x00]));
        let mut s = ListenSession::new();
        s.poll(&mut t);
        s.poll(&mut t);
        assert_eq!(s.respond(&mut t, &[0x90, 0x00]), Ok(()));
        assert_eq!(
            t.sent.last().map(|s| s.as_slice()),
            Some(&[0x00, 0x00, 0x02, 0x90, 0x00][..])
        );
        assert_eq!(s.credits(), 0);
    }

    #[test]
    fn respond_two_fragments_gate_on_credit_regrant() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        t.push_notification(&data_frame(0x00, &[0x00, 0xA4, 0x04, 0x00]));
        t.late_on_wait.push_back(CREDIT_CONN0.to_vec());
        let mut s = ListenSession::new();
        s.poll(&mut t);
        s.poll(&mut t);
        let rapdu = [0x55u8; 300];
        assert_eq!(s.respond(&mut t, &rapdu), Ok(()));
        let data_sent: std::vec::Vec<_> = t
            .sent
            .iter()
            .filter(|f| !f.is_empty() && f[0] & 0xE0 == 0x00)
            .collect();
        assert_eq!(data_sent.len(), 2);
        assert_eq!(data_sent[0][0], 0x10, "first fragment PBF set");
        assert_eq!(data_sent[0][2], 253);
        assert_eq!(data_sent[1][0], 0x00, "last fragment PBF clear");
        assert_eq!(data_sent[1][2], 47);
        assert!(t.waited_ms >= 10, "credit gate must have waited");
        assert_eq!(s.credits(), 0);
    }

    #[test]
    fn respond_credit_timeout_resets() {
        let mut t = MockTransport::new();
        t.push_notification(&ACTIVATION);
        t.push_notification(&data_frame(0x00, &[0x00, 0xA4, 0x04, 0x00]));
        let mut s = ListenSession::new();
        s.poll(&mut t);
        s.poll(&mut t);
        let rapdu = [0x55u8; 300];
        assert_eq!(s.respond(&mut t, &rapdu), Err(RespondError::CreditTimeout));
        assert!(!s.active());
        assert_eq!(s.credits(), 0);
    }
}
