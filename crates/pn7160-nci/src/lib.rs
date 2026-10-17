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

/// CORE_CONN_CREDITS_NTF OID — NCI 1.0 §4.4.4, Table 102: CORE group
/// (GID 0000b), OID 0110b (0x06). Payload = [Number of Entries (1),
/// {Conn ID (1), Credits (1)}...]. UM11495 Rev 1.8 §5.3.1 Table 10:
/// "the number of credits is limited to 1 on each logical connection"
/// for the PN7160 — every DATA packet sent consumes the only credit, so
/// fragmented TX gates each next fragment on one of these
/// ([`reader::exchange`], [`is_conn_credits_ntf`]).
pub const OID_CORE_CONN_CREDITS: u8 = 0x06;

// --- NXP proprietary group (GID 0xF) — UM11495 Rev 1.8 ----------------------
/// GID 0xF — the proprietary group (NCI §3.3 reserves it for vendor use;
/// the PN7160's GID-0xF commands/notifications are listed in UM11495
/// Annex B, Table 123).
pub const GID_PROP: u8 = 0x0F;

/// OID 0x17 in the proprietary group: `PH_NCI_OID_SYSTEM_WTX` (RF_WTX_NTF),
/// wire header `6F 17 00`.
///
/// UM11495 Rev 1.8 §7.3.4, Table 74 (GID 1111b, OID 0x17, 0 parameters):
/// "Notification indicating that RF communication is in phase of WTX(RTOX)
/// REQ/RESP exchange for longer period of time." Sent with a period set by
/// `READER_FWITOX_NTF_CFG` (Table 99, p.102: extended TLV `0xA0 0x65`,
/// len 2, default 0x1000 × 4096/fc ≈ 302 µs ≈ **1.24 s**) while the remote
/// endpoint keeps extending the waiting time during a DATA exchange.
///
/// Bench 2026-10-13 (issue #151): 12 of these (plen=0) over a 13 s stalled
/// DATA exchange on marginal coupling — the NFCC honors every WTX request
/// (ISO 14443-4 caps one WTX at FWT_MAX ≈ 4.95 s but puts NO limit on
/// consecutive WTX; aborting is PCD/DH policy) and notifies the DH per
/// period. NXP's own stacks answer by extending the DH-side response
/// timer (`nfc_ncif_proc_rf_wtx_ntf`, linux_libnfc-nci nci_defs.h
/// `NCI_MSG_RF_WTX`); this firmware instead consumes and ignores it —
/// [`reader::exchange`](crate::reader::exchange) bounds the total wait
/// DH-side. Note: UM11495 Tables 22/99 (all config params) contain NO
/// chip-side ISO-DEP retry-count or total-WTX-budget TLV — the DH-side
/// bound is the only deterministic mechanism (the only `NBR_RETRIES`
/// field anywhere is `TechDet_NBR_RETRIES`, tag-detector/LPCD only).
pub const NTF_PROP_RF_WTX: u8 = 0x17;

// UM11495: Notification indicating that RF communication is in phase of
// WTX(RTOX) REQ/RESP exchange for longer period of time.
// UM11495: If WTX REQ/RESP exchange phase continues an NCI system
// notification WTX is sent with a period configurable via
// READER_FWITOX_NTF_CFG.

/// Is this frame the PN7160 WTX advisory notification (`6F 17 00`)?
/// See [`NTF_PROP_RF_WTX`] for the UM11495 Rev 1.8 citation; the frame
/// carries no payload (Table 74: 0 parameters).
pub fn is_wtx_ntf(f: &Frame) -> bool {
    f.mt == MT_NTF && f.gid == GID_PROP && f.oid == NTF_PROP_RF_WTX
}

/// Is this frame a CORE_CONN_CREDITS_NTF (`60 06 ..`)? NCI 1.0 §4.4.4:
/// "If the NFCC receives a Data Packet that was subject to credit-based
/// flow control, it needs to tell the DH when the buffer is again
/// available for use. It does this by sending CORE_CONN_CREDITS_NTF."
/// The credit re-grant [`reader::exchange`] waits for between DATA
/// fragments (PN7160: 1 credit per connection, UM11495 §5.3.1
/// Table 10).
pub fn is_conn_credits_ntf(f: &Frame) -> bool {
    f.mt == MT_NTF && f.gid == GID_CORE && f.oid == OID_CORE_CONN_CREDITS
}

// --- DATA packet header bits (NCI §5.4.1; NXP nci_defs.h NCI_CID/PBF) ------
pub const CID_MASK: u8 = 0x0F;
pub const PBF_MASK: u8 = 0x10;

// --- Deactivation types (NCI §6.3.5.1, Table 98) ---------------------------
pub const DEACTIVATE_TYPE_IDLE: u8 = 0x00;
pub const DEACTIVATE_TYPE_SLEEP: u8 = 0x01;
pub const DEACTIVATE_TYPE_SLEEP_AF: u8 = 0x02;
/// RF_DEACTIVATE type DISCOVERY — kernel `NCI_DEACTIVATE_TYPE_DISCOVERY`
/// (torvalds include/net/nfc/nci.h): deactivate the RF interface AND
/// return the NFCC to the DISCOVERY state in one command (issue #105:
/// type IDLE leaves the PN7160 idle with discovery stopped, and a manual
/// RF_DISCOVER re-issue per cycle mutes the chip after ~11 cycles —
/// bench 2026-10-10).
pub const DEACTIVATE_TYPE_DISCOVERY: u8 = 0x03;

// --- RF_DEACTIVATE_NTF reason codes (NCI v2.x §5.2.5.2, Table 81) ----------
// NOTE (issue #109 item 1): the reason table is DISPUTED between sources.
// All sources agree 0x00 = DH-requested deactivation (the answer to our
// OWN RF_DEACTIVATE — never a removal). For RF-link-loss the candidates
// diverge: NXP's production NCI 2.x constant header (nci_defs.h, shipped
// in every Android NXP NFC stack — DH_REQ=0, ENDPOINT_REQ=1,
// RF_LINK_LOSS=2, NFCB_BAD_AFI=3, DH_REQ_FAILED=4) puts it at 0x02, while
// NCI 1.x-derived tables put it at 0x01. The PN7160 is NXP silicon, but
// the bench has not yet captured a hand-verified removal NTF (that
// exercise is exactly what this decode gates). Until that capture, BOTH
// candidates decode as removal: on this firmware's NFC-A-only ISO-DEP
// discovery map an ENDPOINT_REQ (card-emulation/P2P flow) and an
// NFC-B bad-AFI cannot occur, so the wider accept risks no false
// removal. Collapse to the captured value when the bench byte lands.
pub const DEACTIVATE_REASON_DH_REQ: u8 = 0x00;
/// RF-link-loss per NCI 1.x-derived reason tables.
pub const DEACTIVATE_REASON_RF_LINK_LOSS_NCI1X: u8 = 0x01;
/// RF-link-loss per NXP's NCI 2.x nci_defs.h (ENDPOINT_REQ occupies 0x01).
pub const DEACTIVATE_REASON_RF_LINK_LOSS_NXP: u8 = 0x02;

/// Is this RF_DEACTIVATE_NTF reason a link-loss (physical removal)?
/// See the block comment above the [`DEACTIVATE_REASON_DH_REQ`] constants
/// for the disputed-table rationale.
pub fn reason_is_link_loss(reason: u8) -> bool {
    reason == DEACTIVATE_REASON_RF_LINK_LOSS_NCI1X || reason == DEACTIVATE_REASON_RF_LINK_LOSS_NXP
}

/// One decoded NCI frame. GID and OID are kept separate, exactly as they
/// sit on the wire (GID in octet0, OID as full octet1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub mt: u8,
    pub gid: u8,
    pub oid: u8,
    /// Packet Boundary Flag (octet 0 bit 4): true = "The Packet contains
    /// a segment of a Message which is not the last segment" (NCI 1.0
    /// §3.4.3, Table 3). DATA-chain reassembly happens in
    /// [`reader::exchange`].
    pub pbf: bool,
    pub payload: [u8; 255],
    pub len: usize,
}

impl Frame {
    /// Decode a full frame from wire bytes. Returns `None` on a short
    /// buffer or truncated payload. PBF-set (fragmented) packets are
    /// ACCEPTED and flagged via [`Frame::pbf`] — NCI 1.0 §3.4.3/Table 3
    /// makes them a legal DATA-packet shape, and the NFCC→DH direction
    /// MAY use them at any payload length (§3.5). Reassembly of DATA
    /// chains lives in [`reader::exchange`].
    ///
    /// On the wire, one NCI packet arrives as one I2C read sequence per
    /// UM11495 §4.2 (the C driver's nci_read: 3-byte header transaction,
    /// then a LEN-byte payload transaction).
    pub fn decode(buf: &[u8]) -> Option<Frame> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        let mt = buf[0] & 0xE0;
        let pbf = (buf[0] & PBF_MASK) != 0;
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
            pbf,
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

    /// CORE_SET_CONFIG TOTAL_DURATION = 0x01FE ms. TLV per the NCI
    /// configuration-parameter format [num_params][id][len][value] with
    /// a ONE-octet param id — verified against the Linux kernel
    /// (`struct set_config_param { __u8 id; __u8 len; ... }`,
    /// include/net/nfc/nci.h), NXP's own stacks
    /// (`NCI_PARAM_ID_TOTAL_DURATION 0x00` / `NCI_PARAM_LEN_TOTAL_DURATION 2`,
    /// nci_defs.h in both linux_libnfc-nci and NFC_NCIHAL_libnfc-nci) and
    /// UM11495 §6.2. The bytes are the wallet's proven TLV verbatim
    /// (value 0x01FE LE). An earlier session misread this plen=5 form
    /// as "missing the LEN octet" under a two-octet-id assumption and
    /// rewrote it into a rejected 6-byte TLV with a trailing garbage
    /// octet; with the true status octet parsed (ed0ed5e) the ladder
    /// aborted at SET_CONFIG (actdiag 2026-10-11).
    ///
    /// PN7160 proprietary params do NOT use this one-octet-id form:
    /// they ride the same CORE_SET_CONFIG as EXTENDED TLVs
    /// `[Tag=0xA0XX, 2 bytes][Len, 1][Val]` (UM11495 Rev 1.8 §7.2,
    /// Table 22, p.39 — e.g. `READER_FWITOX_NTF_CFG 0xA0 0x65`).
    /// Full extracted param inventory + the definitive "no ISO-DEP
    /// retry/WTX-bound TLV exists" negative: issue #151 research
    /// (UM11495 §13.2 Tables 98–100).
    pub const SET_CONFIG_TOTAL_DURATION: [u8; 8] = [
        MT_CMD,
        OID_CORE_SET_CONFIG,
        0x05,
        0x01,
        0x00,
        0x02,
        0xFE,
        0x01,
    ];

    /// CORE_SET_CONFIG carrying ONE EXTENDED (proprietary) TLV:
    /// `READER_FWITOX_NTF_CFG` (tag `0xA0 0x65`, len 2) = the given
    /// value. GREAT EXPECTATION (reference-locked shape — do not "fix"
    /// without re-verifying against the cited references): UM11495
    /// Rev 1.8 §7.2 Table 22 "Extended TLV for proprietary parameters"
    /// — "Tag = 0xA0XX (2 bytes) | Len (1 byte) | Val (m bytes)" —
    /// riding the same CORE_SET_CONFIG payload form as the standard
    /// TLV (NCI 1.0 §5.1.3: `[num_params][param...]`, plen covers the
    /// WHOLE payload). For m=2: plen = 1 + 3 + 2 = 06 (the standard
    /// sibling above carries plen=05 for its 1+1+1+2 payload).
    ///
    /// ⚠️ RW in E²PROM (UM11495 Table 99: "RW in E²PROM") — persistent
    /// across boots: any probe write MUST be reverted to the default
    /// (`0x1000`) in the same session.
    pub const fn set_config_fwitox(value_le: [u8; 2]) -> [u8; 9] {
        // UM11495: To extend this space, the solution chosen is to
        // define a space of Tags coded on 16 bits, instead of 8 bits.
        // These extended Tags will always start by the value 0xA0,
        // which is the first value available in the Proprietary range.
        [
            MT_CMD,
            OID_CORE_SET_CONFIG,
            0x06,
            0x01,
            0xA0,
            0x65,
            0x02,
            value_le[0],
            value_le[1],
        ]
    }

    /// `READER_FWITOX_NTF_CFG` factory default (UM11495 Rev 1.8
    /// Table 99: default value `0x1000` — "Delay before sending TOX
    /// notification to Host. Unit is 4096/fc (~302us)": 0x1000 × 302 µs
    /// ≈ 1.24 s, the bench-observed WTX-NTF spacing, issue #151).
    pub const FWITOX_DEFAULT: [u8; 2] = [0x10, 0x00];
    // UM11495: Delay before sending TOX notification to Host. Unit is
    // 4096/fc (~302us)

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
///
/// The [`send`](Transport::send) / [`poll_frame`](Transport::poll_frame)
/// pair splits `transact` for latency-sensitive callers (the presence
/// re-arm, 2026-10-10 serve-loop-stall fix): a command is WRITTEN without
/// waiting, and its response is collected on a later poll. NCI v2.3
/// §5.2.5 — the NFCC may send the RF_DEACTIVATE_RSP only after finishing
/// its current RF activity, so the RSP wait is the stall risk, not the
/// write.
pub trait Transport {
    /// Write `cmd` WITHOUT waiting for its response. Returns false on a
    /// link failure. The reply surfaces later via
    /// [`poll_frame`](Transport::poll_frame).
    fn send(&mut self, cmd: &[u8]) -> bool;
    /// Read ONE already-pending frame of any message type, if the link
    /// has data ready (IRQ raised). Never waits.
    fn poll_frame(&mut self) -> Option<Frame>;
    /// Send a command, wait for the reply, return the decoded RSP frame.
    fn transact(&mut self, cmd: &[u8]) -> Option<Frame>;
    /// Read one already-pending notification (no command sent).
    fn drain(&mut self) -> Option<Frame>;
    /// Collect-wait hook (#125): no-op in host mocks; the ESP32 I2C
    /// transport delays for real between power_on activation waits.
    fn wait_ms(&mut self, _ms: u64) {}
}

/// NCI DATA packet framing — NCI §5.4.1: octet 0 = PBF(bit 4) | CONN_ID(bits
/// 3:0), octet 1 = RFU (0), octet 2 = length. Byte-exact against the proven
/// C driver's `nci_send_data` (nucula nci.c), which uses CONN_ID 0.
pub mod driver;
pub mod transport;

pub mod data {
    use super::*;

    /// Max payload of one DATA fragment on this link: 253 octets — the
    /// NXP ISO-DEP alignment clamp (linux_libnfc-nci nci_defs.h
    /// `NCI_ISO_DEP_MAX_INFO` = 253 = 256 − ISO-DEP prologue 1 − CRC
    /// epilogue 2), tighter than both the structural NCI limit (LEN is
    /// one octet, 0–255 — NCI 1.0 §3.4.3) and the PN7160's advertised
    /// Max Data Packet payload 255 (UM11495 Rev 1.8 §5.3.1 Table 10;
    /// RF_INTF_ACTIVATED_NTF carries 0xFF). A 253-octet fragment maps
    /// onto one max-size ISO-DEP I-block (NCI 1.0 §8.3.1, Figure 17).
    pub const MAX_DATA_FRAGMENT: usize = 253;

    pub fn encode(conn_id: u8, payload: &[u8], out: &mut Vec<u8, MAX_FRAME>) -> bool {
        encode_fragment(conn_id, false, payload, out)
    }

    /// One DATA packet — NCI 1.0 §3.4.3 "Format of Data Packets": "Each
    /// Data Packet SHALL have a 3-octet Packet Header" — octet 0 =
    /// MT(000b) | PBF(bit 4) | Conn ID(bits 3:0), octet 1 = RFU (0),
    /// octet 2 = Payload Length (0–255). PBF per Table 3: "1b: The
    /// Packet contains a segment of a Message which is not the last
    /// segment" — set on every fragment except the last.
    pub fn encode_fragment(
        conn_id: u8,
        pbf: bool,
        payload: &[u8],
        out: &mut Vec<u8, MAX_FRAME>,
    ) -> bool {
        out.clear();
        if payload.len() > 255 {
            return false;
        }
        let octet0 = (conn_id & CID_MASK) | if pbf { PBF_MASK } else { 0 };
        out.extend_from_slice(&[octet0, 0, payload.len() as u8])
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

    /// The ladder's RF_DISCOVER command, re-exported for the presence
    /// paths (power_off re-issue + one-shot probe, issue #125).
    pub use super::tx::RF_DISCOVER_PASSIVE_A;

    /// RF_DEACTIVATE(IDLE) — byte-exact vs nci.c `nci_restart_discovery`
    /// stop[] = {NCI_MT_CMD | NCI_GID_RF, NCI_OID_RF_DEACTIVATE, 0x01, 0x00}.
    pub const RF_DEACTIVATE_IDLE: [u8; 4] = [
        MT_CMD | GID_RF,
        OID_RF_DEACTIVATE,
        0x01,
        DEACTIVATE_TYPE_IDLE,
    ];

    /// RF_DEACTIVATE(type=DISCOVERY) — the one-command presence re-arm
    /// (issue #105). Exported for the NON-BLOCKING re-arm path
    /// (2026-10-10 serve-loop-stall fix): the driver writes it via
    /// [`Transport::send`] and collects the RSP on later polls.
    pub const RF_DEACTIVATE_DISCOVERY: [u8; 4] = [
        MT_CMD | GID_RF,
        OID_RF_DEACTIVATE,
        0x01,
        DEACTIVATE_TYPE_DISCOVERY,
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

    /// Drain notifications until a tag appears. Accepts both tag-arrival
    /// shapes: an RF_DISCOVER_NTF, or the RF_INTF_ACTIVATED NTF the
    /// PN7160 emits when it AUTO-ACTIVATES an ISO-DEP tag during
    /// discovery (issue #88 bench evidence 2026-10-10: for auto-activated
    /// tags the chip NEVER sends a DISCOVER NTF — the activation NTF is
    /// the only arrival signal). CORE_CONN_CREDITS and other NTFs are
    /// skipped (the wallet's loop ignores credits the same way).
    pub fn wait_for_discovery<T: Transport>(t: &mut T) -> Option<DiscoverNtf> {
        wait_for_tag(t).map(|(n, _)| n)
    }

    /// [`wait_for_discovery`] plus the ATS carried by an auto-activation
    /// NTF (absent for plain discovery NTFs — the ATS only arrives with
    /// the select response in that flow). [`power_on`](crate::driver::
    /// Pn7160Driver::power_on) uses it as the already-activated
    /// fallback path.
    pub fn wait_for_tag<T: Transport>(t: &mut T) -> Option<(DiscoverNtf, Option<Vec<u8, 32>>)> {
        for _ in 0..8 {
            let f = t.drain()?;
            if let Some(n) = DiscoverNtf::decode(&f) {
                return Some((n, None));
            }
            if let Some(n) = activation_to_discover(&f) {
                let ats = activation_ats(&f);
                return Some((n, ats));
            }
        }
        None
    }

    /// What a presence drain saw (issue #88). `LinkLost` is the
    /// RF_DEACTIVATE_NTF with reason RF-link-lost (NCI §5.2.5.2,
    /// Table 81) — the removal signal for an AUTO-ACTIVATED tag: while
    /// the tag sits in the live RF interface, its physical removal tears
    /// the link and the chip announces it. (Resting-in-discovery tags
    /// emit nothing on removal — that absence-of-signal was the original
    /// #88 bug.)
    pub enum TagEvent {
        Tag(DiscoverNtf, Option<Vec<u8, 32>>),
        LinkLost,
    }

    /// Classify ONE frame as a presence signal: a discovery NTF, an
    /// auto-activation NTF (with its ATS), or the link-loss deactivation
    /// of an auto-activated tag (NCI §5.2.5.2, Table 81). `None` = not a
    /// presence signal (RSP frames, CORE_CONN_CREDITS, DH-requested
    /// deactivations — reason 0x00, the answer to our own RF_DEACTIVATE).
    pub fn event_of(f: &Frame) -> Option<TagEvent> {
        if let Some(n) = DiscoverNtf::decode(f) {
            return Some(TagEvent::Tag(n, None));
        }
        if let Some(n) = activation_to_discover(f) {
            return Some(TagEvent::Tag(n, activation_ats(f)));
        }
        if f.mt == MT_NTF
            && f.gid == GID_RF
            && f.oid == NTF_RF_DEACTIVATE
            && f.len >= 2
            && reason_is_link_loss(f.payload[1])
        {
            return Some(TagEvent::LinkLost);
        }
        None
    }

    /// Presence drain accepting all three signals: a discovery NTF, an
    /// auto-activation NTF (with its ATS), or the link-loss deactivation
    /// of an auto-activated tag.
    pub fn wait_for_event<T: Transport>(t: &mut T) -> Option<TagEvent> {
        for _ in 0..8 {
            let f = t.drain()?;
            if let Some(ev) = event_of(&f) {
                return Some(ev);
            }
        }
        None
    }

    /// Synthesize a [`DiscoverNtf`] from the PN7160's auto-activation
    /// RF_INTF_ACTIVATED NTF (issue #88): NCI 2.0 layout (kernel
    /// `struct nci_rf_intf_activated_ntf`, torvalds `include/net/nfc/nci.h`)
    /// = [DiscoveryID, Interface, Protocol, TechAndMode, MaxDataPayload,
    /// InitialNumCredits, RF-tech-params-len, RF-tech-params, DataExch(3),
    /// Activation-params-len, ActivationParams…]. For NFC-A ISO-DEP the
    /// tech params carry [SENS_RES(2), NFCID1_LEN, NFCID1, …] — re-emitted
    /// in the DISCOVER_NTF shape so [`nfca_uid`] keeps working.
    pub fn activation_to_discover(f: &Frame) -> Option<DiscoverNtf> {
        if f.mt != MT_NTF || f.gid != GID_RF || f.oid != NTF_RF_INTF_ACTIVATED || f.len < 5 {
            return None;
        }
        let mut tech_params = [0u8; 16];
        let mut tech_params_len = 0usize;
        if f.len >= 8 {
            let v = tech_params_slice(f)?;
            let uid_len = v[2] as usize;
            if uid_len > 0 && 3 + uid_len <= v.len() {
                let copy = (3 + uid_len).min(16);
                tech_params[..copy].copy_from_slice(&v[..copy]);
                tech_params_len = copy;
            }
        }
        Some(DiscoverNtf {
            discovery_id: f.payload[0],
            protocol: f.payload[2],
            tech_and_mode: f.payload[3],
            interface: f.payload[1],
            tech_params,
            tech_params_len,
        })
    }

    /// ATS (RATS response) from an auto-activation NTF's Activation
    /// Parameters — the NCI 2.0 tail after the tech params and the three
    /// data-exchange octets: bench frame (2026-10-10) ends
    /// `… 0x0A, 0x09, 0x78, 0x77, 0x91, 0x02, 0x80, 0x73, 0xC8, 0x21, 0x10`
    /// (len 0x0A, then the ATS, TL=0x09; `ats_to_atr` bounds the body by
    /// the TL so a trailing octet is harmless).
    pub fn activation_ats(f: &Frame) -> Option<Vec<u8, 32>> {
        if f.mt != MT_NTF || f.gid != GID_RF || f.oid != NTF_RF_INTF_ACTIVATED {
            return None;
        }
        let v = tech_params_slice(f)?;
        let ats_len_pos = 7 + v.len() + 3;
        if f.len <= ats_len_pos {
            return None;
        }
        let ats_len = f.payload[ats_len_pos] as usize;
        if ats_len == 0 || ats_len_pos + 1 + ats_len > f.len {
            return None;
        }
        let mut ats = Vec::new();
        ats.extend_from_slice(&f.payload[ats_len_pos + 1..ats_len_pos + 1 + ats_len])
            .ok()?;
        // The activation tail is a nested TLV (bench fixture, P71:
        // [outer=0x0A][inner=0x09][ATS..]) — the inner length byte
        // rides along in the slice; strip it so the ATS starts at T0.
        if ats.len() >= 2 && ats[0] as usize + 1 == ats.len() {
            let stripped = Vec::from_slice(&ats[1..]).ok()?;
            return Some(stripped);
        }
        Some(ats)
    }

    /// The RF-technology-specific parameters region of an NCI 2.0
    /// activation NTF: length at payload[6], body from payload[7].
    fn tech_params_slice(f: &Frame) -> Option<&[u8]> {
        let tlen = f.payload[6] as usize;
        if 7 + tlen > f.len {
            return None;
        }
        Some(&f.payload[7..7 + tlen])
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

    /// Outcome of [`exchange_detailed`] (issue #156 A2).
    ///
    /// The Data variant carries the same inline 1 KiB heapless buffer
    /// the previous `Option<Vec<u8, 1024>>` return did — identical
    /// stack cost, no allocator to box it in (no_std).
    #[allow(clippy::large_enum_variant)]
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ExchangeOutcome {
        /// Complete response payload (PBF chain reassembled).
        Data(Vec<u8, 1024>),
        /// The 2.4 s collect budget expired (or a DATA write failed on
        /// a mute link) with no completed response: the RF interface
        /// may STILL be live and the chip still honoring WTX — the
        /// driver must resync (deactivate + drain + clear) so the late
        /// response cannot land in the next command's poll window.
        Timeout,
        /// RF_DEACTIVATE_NTF(reason = RF link lost) arrived
        /// mid-exchange: the tag left the field and the chip already
        /// tore the RF interface down itself.
        LinkLost,
    }

    /// Stall callback for [`exchange_detailed`] (issue #156 B): receives
    /// the NOMINAL elapsed collection wait in ms. The CCID layer uses it
    /// to emit interim RDR_to_PC_DataBlock time-extension frames (CCID
    /// Rev 1.1 §6.1.4 — the interim keeps the in-flight XfrBlock's
    /// response type).
    pub type StallHook = fn(elapsed_ms: u32);

    /// Collect rounds per exchange: 12 × 200 ms ≈ 2.4 s, under the
    /// serial host's 3 s read timeout (libccidtwin
    /// DEFAULT_COM_READ_TIMEOUT, reference/CCID/src/defs.h:76).
    const DATA_COLLECT_ROUNDS: usize = 12;

    /// Nominal wait per collect round (host mocks make `wait_ms` a
    /// no-op, so elapsed is tracked as rounds × this value).
    pub const DATA_COLLECT_WAIT_MS: u64 = 200;

    /// Stall-report cadence (issue #156 B): the stall hook fires once
    /// the nominal elapsed crosses 1.5 s and every 1.5 s after —
    /// comfortably inside libccidtwin's 3 s DEFAULT_COM_READ_TIMEOUT,
    /// so an interim CCID time-extension frame always reaches the host
    /// before its select() expires.
    const STALL_REPORT_MS: u32 = 1500;

    /// Strict-RSP collect budget for [`transact_strict`]: 20 polls ×
    /// 10 ms = 200 ms — parity with the firmware I2C transport's
    /// blocking transact IRQ budget (`IRQ_WAIT_BUDGET_US`,
    /// pn7160_i2c.rs; a FreeRtos 1 ms request rounds up to a 10 ms
    /// tick, so the ESP32 wall budget matches). Host mocks no-op
    /// `wait_ms` and just spend the polls.
    pub const STRICT_RSP_POLLS: usize = 20;
    const STRICT_RSP_WAIT_MS: u64 = 10;

    /// Cap for [`drain_link`] (Codex PR159 P1): the teardown/resync
    /// path can queue several frames — a late APDU DATA response
    /// (possibly a PBF chain of ~5 packets on a 1 KiB exchange), the
    /// RF_DEACTIVATE_NTF, a fallback RF_DISCOVER_RSP, WTX advisories —
    /// and the cap bounds a chatty or wedged chip that streams frames
    /// faster than the loop reads (the semi-muted PN7160 keeps I2C
    /// ACKing — bench 2026-10-12).
    pub const DRAIN_LINK_CAP: usize = 16;

    /// Exchange one APDU over the RF data connection: TX fragmentation,
    /// credit gating between fragments, RX reassembly of the reply
    /// chain. The reply must be DATA packet(s) on the same connection
    /// (Frame::decode surfaces DATA packets with mt == MT_DATA and
    /// gid == conn_id).
    ///
    /// TX (NCI 1.0 §3.5 "Segmentation and Reassembly"): the APDU is
    /// chunked at [`data::MAX_DATA_FRAGMENT`] = 253 octets — "For each
    /// segment of a Data Message, the header of the Data Packet SHALL
    /// contain the same MT and Conn ID" — with PBF=1 on every fragment
    /// except the last (Table 3: "1b: The Packet contains a segment of a
    /// Message which is not the last segment"). Between fragments the
    /// exchange waits for a CORE_CONN_CREDITS_NTF (§4.4.4: "The DH SHALL
    /// NOT send any Data Packet on a connection when the corresponding
    /// NFCC_Credits_Avail is 0"): the PN7160 grants exactly 1 credit per
    /// logical connection (UM11495 Rev 1.8 §5.3.1 Table 10), so every
    /// DATA packet consumes the only credit and fragment 2 stalls the
    /// NFCC until the re-grant arrives.
    ///
    /// RX (NCI 1.0 §3.5): "When an NFCC sends a Data Message to the DH,
    /// regardless of the payload length, it MAY segment the Data Message
    /// into smaller Data Packets for any internal reason" — DATA packets
    /// on the connection are concatenated until one arrives with PBF=0
    /// (§8.3.1.2: the last byte of a non-chaining I-block's payload
    /// "SHALL be the last octet of the Data Packet with Packet Boundary
    /// Flag set to 0b").
    ///
    /// Bench 2026-10-13: on marginal coupling the NFCC retries at the RF
    /// layer for SECONDS before the DATA response arrives (a 13 s stall
    /// with proprietary retry NTFs was observed mid-GP-auth) — the old
    /// single 200 ms transact gave up instantly. Fire the request
    /// (non-blocking send) and collect with a bounded wait: 12 × 200 ms
    /// ≈ 2.4 s, under the serial host's 3 s read timeout
    /// (libccidtwin DEFAULT_COM_READ_TIMEOUT), so the host always gets
    /// our answer — success or a clean failure — before its select()
    /// expires. That budget is shared across the WHOLE exchange
    /// (fragment credit gates + reply collection). Non-matching frames
    /// are consumed and ignored, matching the driver's neutral handling
    /// — in practice the churn is the PN7160's WTX advisory `6F 17 00`
    /// ([`is_wtx_ntf`]; UM11495 Rev 1.8 §7.3.4 Table 74 — sent about
    /// every `READER_FWITOX_NTF_CFG` period, default ≈1.24 s, while the
    /// WTX phase continues) plus CORE_CONN_CREDITS. NXP's own DH extends
    /// its response timer on each WTX NTF; we deliberately do NOT — the
    /// 2.4 s budget above is the bound (the chip offers no retry-bound
    /// TLV to tighten, see [`NTF_PROP_RF_WTX`]).
    pub fn exchange<T: Transport>(t: &mut T, conn_id: u8, apdu: &[u8]) -> Option<Vec<u8, 1024>> {
        match exchange_detailed(t, conn_id, apdu, None) {
            (ExchangeOutcome::Data(v), _) => Some(v),
            _ => None,
        }
    }

    /// Fire-and-collect DATA exchange with a detailed outcome (issue
    /// #156 A2): distinguishes a collection TIMEOUT (the #151 desync
    /// hole — the RF interface is still live and the chip still honors
    /// WTX; the late DATA response would land in the NEXT command's
    /// poll window) from a mid-exchange LINK LOSS (the tag left the
    /// field; the chip already tore the interface down itself).
    ///
    /// `stall` (issue #156 B): invoked with the NOMINAL elapsed wait
    /// (waited rounds × [`DATA_COLLECT_WAIT_MS`]) each time the
    /// exchange has been collecting without a completed response for
    /// ~1.5 s and every ~1.5 s after that — the caller emits CCID
    /// time-extension frames from it. With the current 2.4 s budget at
    /// most ONE emission lands per exchange (~1.6 s in); a future
    /// larger budget extends at the same cadence.
    ///
    /// A DATA-packet send failure (mute/NAK link) is reported as
    /// [`ExchangeOutcome::Timeout`]: the driver's resync teardown is
    /// the safe response to an unknown chip state either way.
    pub fn exchange_detailed<T: Transport>(
        t: &mut T,
        conn_id: u8,
        apdu: &[u8],
        stall: Option<StallHook>,
    ) -> (ExchangeOutcome, u32) {
        let mut budget = DATA_COLLECT_ROUNDS;
        let mut waited_rounds: usize = 0;
        let mut wtx: u32 = 0;
        // Stall cadence: first report once the nominal elapsed crosses
        // 1.5 s, then every 1.5 s after (under libccidtwin's 3 s
        // DEFAULT_COM_READ_TIMEOUT — reference/CCID/src/defs.h:76).
        let mut next_stall_ms: u32 = STALL_REPORT_MS;

        // One waited round completed without a response: count WTX
        // churn, advance the stall clock.
        fn round_waited<T: Transport>(
            t: &mut T,
            waited_rounds: &mut usize,
            next_stall_ms: &mut u32,
            stall: Option<StallHook>,
        ) {
            *waited_rounds += 1;
            let elapsed = (*waited_rounds as u32) * (DATA_COLLECT_WAIT_MS as u32);
            if elapsed >= *next_stall_ms {
                if let Some(hook) = stall {
                    hook(elapsed);
                }
                *next_stall_ms = (*next_stall_ms).saturating_add(STALL_REPORT_MS);
            }
            t.wait_ms(DATA_COLLECT_WAIT_MS);
        }

        // TX: PBF-chained fragments, credit-gated (NCI 1.0 §3.5).
        if apdu.is_empty() {
            // "In the case of an 'empty' Data Message, only the Packet
            // Header is sent" (NCI 1.0 §3.4.3 NOTE) — the historical
            // single empty DATA packet.
            let mut frame = Vec::new();
            if !data::encode_fragment(conn_id, false, &[], &mut frame) || !t.send(&frame) {
                return (ExchangeOutcome::Timeout, wtx);
            }
        }
        let mut fragments = apdu.chunks(data::MAX_DATA_FRAGMENT).peekable();
        while let Some(fragment) = fragments.next() {
            let last = fragments.peek().is_none();
            let mut frame = Vec::new();
            if !data::encode_fragment(conn_id, !last, fragment, &mut frame) || !t.send(&frame) {
                return (ExchangeOutcome::Timeout, wtx);
            }
            if last {
                break;
            }
            // Credit gate: the fragment just sent consumed the PN7160's
            // single credit (UM11495 §5.3.1 Table 10) — collect frames
            // until the CORE_CONN_CREDITS_NTF re-grant (NCI 1.0 §4.4.4);
            // churn (WTX advisories) is consumed while waiting.
            let mut credited = false;
            while budget > 0 {
                if let Some(f) = t.poll_frame() {
                    if is_wtx_ntf(&f) {
                        wtx += 1;
                    }
                    if matches!(event_of(&f), Some(TagEvent::LinkLost)) {
                        return (ExchangeOutcome::LinkLost, wtx);
                    }
                    if is_conn_credits_ntf(&f) {
                        credited = true;
                        break;
                    }
                }
                budget -= 1;
                round_waited(t, &mut waited_rounds, &mut next_stall_ms, stall);
            }
            if !credited {
                return (ExchangeOutcome::Timeout, wtx);
            }
        }

        // RX: collect DATA packets on the connection, reassembling the
        // PBF chain (NCI 1.0 §3.5; last packet carries PBF=0).
        let mut response: Vec<u8, 1024> = Vec::new();
        while budget > 0 {
            if let Some(f) = t.poll_frame() {
                if is_wtx_ntf(&f) {
                    wtx += 1;
                }
                if matches!(event_of(&f), Some(TagEvent::LinkLost)) {
                    return (ExchangeOutcome::LinkLost, wtx);
                }
                if f.mt == MT_DATA && f.gid == conn_id && f.len > 0 {
                    if response.extend_from_slice(&f.payload[..f.len]).is_err() {
                        // reassembly overran the 1024-octet budget
                        return (ExchangeOutcome::Timeout, wtx);
                    }
                    if !f.pbf {
                        return (ExchangeOutcome::Data(response), wtx);
                    }
                }
            }
            budget -= 1;
            round_waited(t, &mut waited_rounds, &mut next_stall_ms, stall);
        }
        (ExchangeOutcome::Timeout, wtx)
    }

    /// Send `cmd` and collect its RSP with STRICT matching (Codex
    /// PR159 P1): non-matching frames on the link — a late APDU DATA
    /// packet, WTX advisories, notifications — are consumed as churn,
    /// and only the frame that answers THIS command
    /// ([`Frame::is_rsp_to`]) terminates the wait.
    ///
    /// [`Transport::transact`] cannot offer this contract: its
    /// "first non-NTF frame" rule (the firmware I2C transport,
    /// pn7160_i2c.rs) is exactly what lets a late DATA packet
    /// masquerade as the RF_DEACTIVATE_RSP — the real RSP then stays
    /// pending and cascades mismatches through the teardown fallback
    /// chain. Budget: [`STRICT_RSP_POLLS`] × 10 ms (see the const).
    pub fn transact_strict<T: Transport>(t: &mut T, cmd: &[u8]) -> Result<Frame, &'static str> {
        if !t.send(cmd) {
            return Err("write failed");
        }
        for round in 0..STRICT_RSP_POLLS {
            if round > 0 {
                t.wait_ms(STRICT_RSP_WAIT_MS);
            }
            if let Some(f) = t.poll_frame() {
                if f.is_rsp_to(cmd) {
                    return Ok(f);
                }
            }
        }
        Err("no matching response")
    }

    /// Drain the transport until it is actually EMPTY, bounded by
    /// [`DRAIN_LINK_CAP`] (Codex PR159 P1): one `drain()` reads
    /// exactly ONE pending frame, but the teardown/resync path can
    /// queue several — any survivor lands in the next command's poll
    /// window (#101-class desync). The cap returns control on a chatty
    /// or wedged chip instead of looping forever.
    pub fn drain_link<T: Transport>(t: &mut T) {
        for _ in 0..DRAIN_LINK_CAP {
            if t.drain().is_none() {
                return;
            }
        }
    }

    /// #170: teardown drain with a settle beat. The NFCC keeps honoring
    /// the card's WTX past the DH's exchange budget (bench 2026-10-13:
    /// a 13 s stall) — the late DATA response can land AFTER the drain
    /// burst, queue-empty at that instant. Drain, wait one collect
    /// round, drain again: a frame arriving during the settle is
    /// consumed here instead of answering the NEXT command's poll
    /// window.
    pub fn settle_drain<T: Transport>(t: &mut T) {
        drain_link(t);
        t.wait_ms(DATA_COLLECT_WAIT_MS);
        drain_link(t);
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

    /// Deactivate the RF interface straight back into the DISCOVERY
    /// state (RF_DEACTIVATE type DISCOVERY, NCI §5.2 / kernel
    /// NCI_DEACTIVATE_TYPE_DISCOVERY) — the one-command presence re-arm:
    /// discovery resumes natively, so a tag still in the field is
    /// re-found (auto-activated ISO-DEP tags re-emit INTF_ACTIVATED
    /// within ~TOTAL_DURATION) without stacking RF_DISCOVER re-issues.
    /// BLOCKING (bounded wait for the RSP); the presence path uses the
    /// split [`Transport::send`] + [`Transport::poll_frame`] form
    /// instead. RSP matching is STRICT ([`transact_strict`], Codex
    /// PR159 P1): a late DATA packet pending on the link is consumed
    /// as churn, never mistaken for the deactivate RSP.
    pub fn deactivate_to_discovery<T: Transport>(t: &mut T) -> Result<(), &'static str> {
        let cmd = RF_DEACTIVATE_DISCOVERY;
        let rsp = transact_strict(t, &cmd).map_err(|_| "no deactivate response")?;
        if rsp.status() != Some(STATUS_OK) {
            return Err("deactivate status != OK");
        }
        // The DEACTIVATE_NTF is optional timing-wise; a missing one is
        // not fatal — discovery restart is what matters. Drain to
        // EMPTY, not one frame: the teardown can leave several pending
        // (Codex PR159 P1).
        drain_link(t);
        Ok(())
    }

    /// Re-issue RF_DISCOVER — kernel `nci_start_poll` semantics
    /// (torvalds net/nfc/nci/rsp.c: the DH owns the polling loop). Bench
    /// 2026-10-11 (#125): after a session teardown the bench PN7160 does
    /// NOT resume polling on its own — not even a type-DISCOVERY
    /// deactivation restarts it — so presence (GSS bStatus) reads absent
    /// forever while the card sits on the coil. A fresh RF_DISCOVER puts
    /// the NFCC back in the discovery loop; an ISO-DEP tag in the field
    /// auto-activates and re-emits RF_INTF_ACTIVATED, re-latching
    /// presence. ONE-SHOT per teardown: a per-cycle re-issue mutes the
    /// chip after ~11 cycles (bench 2026-10-10, see
    /// [`DEACTIVATE_TYPE_DISCOVERY`]). RSP matching is STRICT
    /// ([`transact_strict`], Codex PR159 P1) — same masquerade hazard
    /// as the deactivate it falls back from.
    pub fn restart_discovery<T: Transport>(t: &mut T) -> Result<(), &'static str> {
        let cmd = RF_DISCOVER_PASSIVE_A;
        let rsp = transact_strict(t, &cmd).map_err(|_| "no discover response")?;
        if rsp.status() != Some(STATUS_OK) {
            return Err("discover status != OK");
        }
        Ok(())
    }
}

/// Bring-up steps, in C-driver order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    CoreReset,
    CoreInit,
    SetConfigTotalDuration,
    RfDiscoverMap,
    RfDiscover,
}

impl Step {
    pub fn tx_bytes(self) -> &'static [u8] {
        match self {
            Step::CoreReset => &tx::CORE_RESET,
            Step::CoreInit => &tx::CORE_INIT,
            Step::SetConfigTotalDuration => &tx::SET_CONFIG_TOTAL_DURATION,
            Step::RfDiscoverMap => &tx::RF_DISCOVER_MAP_ISO_DEP,
            Step::RfDiscover => &tx::RF_DISCOVER_PASSIVE_A,
        }
    }

    /// Advisory steps set optional NFCC parameters. The bench PN7160
    /// answers SET_CONFIG with status=REJECTED / num_applied=0
    /// (actdiag 2026-10-11: I2C alive, ladder aborts at the first
    /// SET_CONFIG once the status octet is read correctly — ed0ed5e)
    /// and STILL runs ISO-DEP discovery; the reader worked for weeks
    /// with the pre-ed0ed5e parser masking the rejection. Re-discovery
    /// cadence is owned by the firmware re-arm state machine (5f702e4),
    /// not TOTAL_DURATION, so a rejected config is survivable.
    pub const fn advisory(self) -> bool {
        matches!(self, Step::SetConfigTotalDuration)
    }

    /// Static error string for a failed load-bearing step — the generic
    /// "step status != OK" hid WHICH step failed and cost a bench
    /// session to find (actdiag 2026-10-11).
    pub const fn status_err(self) -> &'static str {
        match self {
            Step::CoreInit => "CoreInit status != OK",
            Step::RfDiscoverMap => "RfDiscoverMap status != OK",
            Step::RfDiscover => "RfDiscover status != OK",
            _ => "step status != OK",
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
///  4. NO CORE_SET_CONFIG: UM11495's TOTAL_DURATION example (510 ms)
///     makes discovery SINGLE-SHOT — polling dies ~510 ms in and
///     presence never re-arms after a power-off (bench 2026-10-11:
///     pcscd "Card removed" while the card sat on the coil). The NXP
///     default (perpetual discovery) is what a reader wants; the bench
///     ran weeks on it. [`Step::SetConfigTotalDuration`] and the
///     reference-locked TLV stay for documentation/tests only.
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
    for step in [Step::CoreInit, Step::RfDiscoverMap, Step::RfDiscover] {
        let rsp = t.transact(step.tx_bytes()).ok_or("transport ran dry")?;
        if !rsp.is_rsp_to(step.tx_bytes()) {
            return Err("response does not match command");
        }
        // NCI v2.x §5.1.3 (CORE_SET_CONFIG_RSP): payload =
        // [status, num_params, param_id...]. ALL CORE responses lead
        // with the status octet (see Frame::status() — same rule).
        // The previous special case read payload[1] (num_params) as
        // status and pushed mock payloads [0x01, 0x00] — modeling a
        // REJECTED config as success. Codex PR 85 flagged it; the
        // file's own doc (Frame::status, "SET_CONFIG_RSP … lead with
        // status") contradicted the special case.
        if rsp.status() != Some(STATUS_OK) {
            if step.advisory() {
                continue;
            }
            return Err(step.status_err());
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

    #[derive(Default)]
    pub struct MockTransport {
        pub sent: StdVec<StdVec<u8>>,
        pub replies: VecDeque<StdVec<u8>>,
        pub notifications: VecDeque<StdVec<u8>>,
        pub drained: usize,
        /// RSP parked by `send`, surfacing on the next `poll_frame` —
        /// models the chip answering (IRQ rise) shortly after the
        /// command write, without the caller blocking.
        pub pending_rsp: Option<StdVec<u8>>,
        /// Blocking `transact` call count — lets tests assert the
        /// presence path never waits for a reply.
        pub transacts: usize,
        /// Total ms passed to `wait_ms` — proves settle beats happened.
        pub waited_ms: u64,
        /// Frames materializing on `wait_ms` calls (one per call, front
        /// first) — models a WTX-honoring chip delivering a response
        /// AFTER the caller's drain burst found the link empty (#170:
        /// the settle-drain contract).
        pub late_on_wait: VecDeque<StdVec<u8>>,
    }

    impl MockTransport {
        pub fn new() -> Self {
            MockTransport {
                sent: StdVec::new(),
                replies: VecDeque::new(),
                notifications: VecDeque::new(),
                drained: 0,
                pending_rsp: None,
                transacts: 0,
                waited_ms: 0,
                late_on_wait: VecDeque::new(),
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
        fn send(&mut self, cmd: &[u8]) -> bool {
            self.sent.push(cmd.to_vec());
            if let Some(raw) = self.replies.pop_front() {
                self.pending_rsp = Some(raw);
            }
            true
        }
        fn poll_frame(&mut self) -> Option<Frame> {
            if let Some(raw) = self.pending_rsp.take() {
                return Frame::decode(&raw);
            }
            let f = self
                .notifications
                .pop_front()
                .and_then(|raw| Frame::decode(&raw));
            if f.is_some() {
                self.drained += 1;
            }
            f
        }
        fn transact(&mut self, cmd: &[u8]) -> Option<Frame> {
            self.transacts += 1;
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
        fn wait_ms(&mut self, ms: u64) {
            self.waited_ms += ms;
            if let Some(raw) = self.late_on_wait.pop_front() {
                self.notifications.push_back(raw);
            }
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
    fn decode_rejects_short_accepts_pbf() {
        assert!(Frame::decode(&[0x40, 0x00]).is_none());
        assert!(Frame::decode(&[0x40, 0x00, 0x02, 0x11]).is_none()); // short payload
                                                                     // NCI 1.0 §3.4.3/Table 3: PBF set (octet 0 bit 4) = "The Packet
                                                                     // contains a segment of a Message which is not the last segment"
                                                                     // — a legal DATA-packet shape now ACCEPTED (reassembly:
                                                                     // reader::exchange). decode surfaces the flag instead of
                                                                     // rejecting; the flag is what RX chaining keys on.
        let frag = Frame::decode(&[0x50, 0x00, 0x00]).unwrap();
        assert!(frag.pbf);
        let complete = Frame::decode(&[0x40, 0x00, 0x00]).unwrap();
        assert!(!complete.pbf);
        // DATA-shaped fragment: octet0 = MT(000b)|PBF|ConnID.
        let data_frag = Frame::decode(&[0x10, 0x00, 0x02, 0xAA, 0xBB]).unwrap();
        assert!(data_frag.pbf);
        assert_eq!(data_frag.mt, MT_DATA);
        assert_eq!(data_frag.gid, 0);
        assert_eq!(&data_frag.payload[..2], &[0xAA, 0xBB]);
    }

    #[test]
    fn tx_frames_match_c_driver_bytes() {
        assert_eq!(&tx::CORE_RESET, &[0x20, 0x00, 0x01, 0x01]);
        assert_eq!(&tx::CORE_INIT, &[0x20, 0x01, 0x02, 0x00, 0x00]);
        // Reference: one-octet param id 0x00 (TOTAL_DURATION), len 2,
        // value 0x01FE LE — wallet bytes, kernel struct + NXP nci_defs.h
        // format (include/net/nfc/nci.h, NCI_PARAM_ID_TOTAL_DURATION).
        assert_eq!(
            &tx::SET_CONFIG_TOTAL_DURATION,
            &[0x20, 0x02, 0x05, 0x01, 0x00, 0x02, 0xFE, 0x01]
        );
        // GREAT EXPECTATION (reference-locked bytes — do not "fix"
        // without re-verifying against the cited references):
        // bench-validated 2026-10-11 on the PN7160 (actdiag ladder OK,
        // SET_CONFIG RSP [status=OK, 0 failed], RF_INTF_ACTIVATED at
        // +943 ms with the card on coil). DISCOVER_MAP entry order is
        // NCI 2.0 [protocol, interface, mode] — NOT the NCI 1.x / NXP
        // legacy [protocol, mode, interface] order.
        assert_eq!(
            &tx::RF_DISCOVER_MAP_ISO_DEP,
            &[0x21, 0x00, 0x04, 0x01, 0x04, 0x02, 0x01]
        );
        assert_eq!(
            &tx::RF_DISCOVER_PASSIVE_A,
            &[0x21, 0x03, 0x03, 0x01, 0x00, 0x01]
        );
        // GREAT EXPECTATION (extended-TLV probe bytes, issue #156 C —
        // derived from Table 22 + the NCI §5.1.3 payload form locked by
        // SET_CONFIG_TOTAL_DURATION above; plen=06 covers
        // [num=1][tag A0 65][len 02][val]):
        assert_eq!(
            &tx::set_config_fwitox([0x02, 0x00]),
            &[0x20, 0x02, 0x06, 0x01, 0xA0, 0x65, 0x02, 0x02, 0x00]
        );
        assert_eq!(
            &tx::set_config_fwitox(tx::FWITOX_DEFAULT),
            &[0x20, 0x02, 0x06, 0x01, 0xA0, 0x65, 0x02, 0x10, 0x00]
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
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER_MAP, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER, &[0x00]));

        run_ladder(&mut t).expect("ladder should pass");
        assert_eq!(t.sent.len(), 4);
        assert_eq!(t.drained, 1);
    }

    #[test]
    fn ladder_tolerates_reset_ntf_only() {
        let mut t = mock::MockTransport::new();
        t.push_reply(&rsp(0x40, OID_CORE_RESET, &[0x00]));
        t.push_notification(&[0x60, OID_CORE_RESET, 0x01, 0x00]);
        t.push_reply(&rsp(0x40, OID_CORE_INIT, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER_MAP, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER, &[0x00]));

        run_ladder(&mut t).expect("ladder should pass with reset NTF");
        assert_eq!(t.sent.len(), 4);
        assert_eq!(t.drained, 1);
    }

    #[test]
    fn ladder_stops_on_bad_init_status() {
        let mut t = mock::MockTransport::new();
        t.push_reply(&rsp(0x40, OID_CORE_RESET, &[0x00]));
        t.push_notification(&[0x60, OID_CORE_RESET, 0x01, 0x00]);
        t.push_reply(&rsp(0x40, OID_CORE_INIT, &[0x02]));
        assert_eq!(run_ladder(&mut t), Err("CoreInit status != OK"));
    }

    #[test]
    fn ladder_tolerates_set_config_rejection() {
        // Chip reality (actdiag 2026-10-11): the PN7160 answers both
        // SET_CONFIGs with status=REJECTED / num_applied=0 and still
        // runs discovery. Advisory steps must not abort the ladder.
        let mut t = mock::MockTransport::new();
        t.push_reply(&rsp(0x40, OID_CORE_RESET, &[0x00]));
        t.push_notification(&[0x60, OID_CORE_RESET, 0x01, 0x00]);
        t.push_reply(&rsp(0x40, OID_CORE_INIT, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER_MAP, &[0x00]));
        t.push_reply(&rsp(0x41, OID_RF_DISCOVER, &[0x00]));

        run_ladder(&mut t).expect("SET_CONFIG rejection must not abort");
        assert_eq!(t.sent.len(), 4);
        assert_eq!(t.drained, 1);
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
            pbf: false,
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
    fn deactivate_ntf_reason_classification() {
        // Issue #109 item 1: reason 0x00 = DH-requested deactivation (the
        // answer to our own RF_DEACTIVATE) — neutral, never a removal.
        // Both link-loss candidates (disputed Table 81, see the
        // DEACTIVATE_REASON_* block comment) are removals.
        let dh = ntf_frame(
            NTF_RF_DEACTIVATE,
            &[DEACTIVATE_TYPE_DISCOVERY, DEACTIVATE_REASON_DH_REQ],
        );
        assert!(reader::event_of(&dh).is_none(), "DH-request ≠ removal");

        let loss_1x = ntf_frame(
            NTF_RF_DEACTIVATE,
            &[DEACTIVATE_TYPE_IDLE, DEACTIVATE_REASON_RF_LINK_LOSS_NCI1X],
        );
        assert!(matches!(
            reader::event_of(&loss_1x),
            Some(reader::TagEvent::LinkLost)
        ));

        let loss_nxp = ntf_frame(
            NTF_RF_DEACTIVATE,
            &[DEACTIVATE_TYPE_IDLE, DEACTIVATE_REASON_RF_LINK_LOSS_NXP],
        );
        assert!(matches!(
            reader::event_of(&loss_nxp),
            Some(reader::TagEvent::LinkLost)
        ));

        // Truncated NTF (no reason octet): neutral, not a removal.
        let short = ntf_frame(NTF_RF_DEACTIVATE, &[DEACTIVATE_TYPE_IDLE]);
        assert!(reader::event_of(&short).is_none());
    }

    #[test]
    fn reason_is_link_loss_table() {
        assert!(!reason_is_link_loss(DEACTIVATE_REASON_DH_REQ));
        assert!(reason_is_link_loss(DEACTIVATE_REASON_RF_LINK_LOSS_NCI1X));
        assert!(reason_is_link_loss(DEACTIVATE_REASON_RF_LINK_LOSS_NXP));
        // NXP tail codes (ENDPOINT_REQ is the 0x01 occupant there, but a
        // bare 0x03 NFC-B bad-AFI / 0x04 DH-failure never reaches us on
        // the NFC-A ISO-DEP-only discovery map): neutral today.
        assert!(!reason_is_link_loss(0x03));
        assert!(!reason_is_link_loss(0x04));
        assert_eq!(DEACTIVATE_REASON_DH_REQ, 0x00);
    }

    #[test]
    fn wtx_ntf_bytes_match_um11495_table_74() {
        // GREAT EXPECTATION (reference-locked bytes — do not "fix"
        // without re-verifying against the cited reference):
        // bench capture 2026-10-13 (issue #151, 12× during a 13 s
        // stalled DATA exchange) and UM11495 Rev 1.8 §7.3.4 Table 74 —
        // PH_NCI_OID_SYSTEM_WTX: GID 1111b (0xF), OID 0x17, zero
        // parameters → wire header exactly 6F 17 00.
        let raw = [0x6F, 0x17, 0x00];
        let f = Frame::decode(&raw).unwrap();
        assert_eq!(f.mt, MT_NTF);
        assert_eq!(f.gid, GID_PROP);
        assert_eq!(f.oid, NTF_PROP_RF_WTX);
        assert_eq!(f.len, 0);
        assert!(is_wtx_ntf(&f));
        // Pure advisory: not a presence signal, not a DATA packet —
        // reader::event_of ignores it and exchange() consumes it as churn.
        assert!(reader::event_of(&f).is_none());
        // Discrimination: RF-group NTFs and GID-0xF *commands* must not
        // classify as WTX (Table 123 keeps GID-0xF 0x17 out of the
        // command set — it exists only as this notification).
        let rf_ntf = ntf_frame(NTF_RF_DEACTIVATE, &[0x00, 0x00]);
        assert!(!is_wtx_ntf(&rf_ntf));
        let wrong_mt = Frame::decode(&[0x2F, 0x17, 0x00]).unwrap();
        assert!(!is_wtx_ntf(&wrong_mt));
    }

    #[test]
    fn exchange_consumes_wtx_ntf_churn_before_data() {
        // Bench shape (issue #151): WTX advisories 6F 17 00 interleave
        // with the DATA response; exchange must consume them and still
        // deliver the DATA packet on the same connection.
        let mut t = mock::MockTransport::new();
        t.push_notification(&[0x6F, 0x17, 0x00]); // WTX churn first
        t.push_notification(&[0x00, 0x00, 0x02, 0x90, 0x00]); // DATA on conn 0
        let rsp = reader::exchange(&mut t, 0, &[0x00, 0xA4, 0x04, 0x00])
            .expect("data must arrive after wtx churn");
        assert_eq!(rsp.as_slice(), &[0x90, 0x00]);
    }

    #[test]
    fn exchange_detailed_distinguishes_timeout_from_link_lost() {
        // Issue #156 A2: an exchange that runs the full collect budget
        // out with no DATA packet is a TIMEOUT (interface may still be
        // live — driver must resync); an RF_DEACTIVATE(link-loss) NTF
        // mid-collection is LINK LOST (chip already tore down).
        let mut t = mock::MockTransport::new();
        let (outcome, wtx) = reader::exchange_detailed(&mut t, 0, &[0x00, 0xA4], None);
        assert_eq!(outcome, reader::ExchangeOutcome::Timeout);
        assert_eq!(wtx, 0, "no advisories in a silent timeout");

        let mut t = mock::MockTransport::new();
        t.push_notification(&[
            MT_NTF | GID_RF,
            NTF_RF_DEACTIVATE,
            0x02,
            0x00,
            DEACTIVATE_REASON_RF_LINK_LOSS_NCI1X,
        ]);
        let (outcome, _) = reader::exchange_detailed(&mut t, 0, &[0x00, 0xA4], None);
        assert_eq!(outcome, reader::ExchangeOutcome::LinkLost);
    }

    #[test]
    fn exchange_detailed_counts_wtx_advisories() {
        // Issue #156 A3: every consumed 6F 17 00 advisory counts toward
        // the exchange's WTX total (bench model: ≈ stall / 1.24 s).
        let mut t = mock::MockTransport::new();
        t.push_notification(&[0x6F, 0x17, 0x00]);
        t.push_notification(&[0x6F, 0x17, 0x00]);
        t.push_notification(&[0x00, 0x00, 0x02, 0x90, 0x00]);
        let (outcome, wtx) = reader::exchange_detailed(&mut t, 0, &[0x00, 0xA4], None);
        assert!(matches!(outcome, reader::ExchangeOutcome::Data(_)));
        assert_eq!(wtx, 2);
    }

    #[test]
    fn exchange_stall_hook_fires_at_1p5s_cadence() {
        // Issue #156 B: the stall hook fires once the nominal elapsed
        // (rounds × 200 ms) crosses 1.5 s, then every 1.5 s after. The
        // 12-round budget (2.4 s) admits exactly ONE emission (round 8
        // → 1600 ms); a round-16 second emission would need a bigger
        // budget.
        use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(usize::MAX);
        static LAST: AtomicU32 = AtomicU32::new(u32::MAX);
        fn recorder(elapsed_ms: u32) {
            LAST.store(elapsed_ms, Ordering::SeqCst);
            CALLS.fetch_add(1, Ordering::SeqCst);
        }
        CALLS.store(0, Ordering::SeqCst);

        let mut t = mock::MockTransport::new();
        let (outcome, _) = reader::exchange_detailed(&mut t, 0, &[0x00, 0xA4], Some(recorder));
        assert_eq!(outcome, reader::ExchangeOutcome::Timeout);
        assert_eq!(CALLS.load(Ordering::SeqCst), 1, "single emission in 2.4 s");
        assert_eq!(LAST.load(Ordering::SeqCst), 1600, "round 8 × 200 ms");
    }

    // given a late APDU DATA packet and advisories are pending ahead of
    // the teardown RSP (the Codex PR159 P1 masquerade), when
    // transact_strict collects the response, then the churn is
    // consumed and ONLY the frame answering the command is returned —
    // `transact`'s "first non-NTF frame" rule would have handed back
    // the DATA packet. The transport scripts the exact hardware frame
    // order (the #1 mock/hardware divergence lesson: model the
    // transport's filtering semantics, not just the happy queue).
    #[test]
    fn transact_strict_skips_late_data_and_wtx_to_the_matching_rsp() {
        struct ChattyTransport {
            polls: usize,
        }
        impl Transport for ChattyTransport {
            fn send(&mut self, _cmd: &[u8]) -> bool {
                true
            }
            fn poll_frame(&mut self) -> Option<Frame> {
                let schedule: [&[u8]; 3] = [
                    &[0x00, 0x00, 0x02, 0x90, 0x00], // late APDU DATA packet
                    &[0x6F, 0x17, 0x00],             // WTX advisory
                    &[0x41, 0x06, 0x01, 0x00],       // RF_DEACTIVATE_RSP OK
                ];
                let raw = *schedule.get(self.polls)?;
                self.polls += 1;
                Frame::decode(raw)
            }
            fn transact(&mut self, _cmd: &[u8]) -> Option<Frame> {
                None
            }
            fn drain(&mut self) -> Option<Frame> {
                None
            }
        }
        let mut t = ChattyTransport { polls: 0 };
        let rsp = reader::transact_strict(&mut t, &reader::RF_DEACTIVATE_DISCOVERY)
            .expect("RSP must surface after the churn");
        assert_eq!(rsp.mt, MT_RSP);
        assert_eq!(rsp.oid, OID_RF_DEACTIVATE);
        assert_eq!(rsp.status(), Some(STATUS_OK));
        assert_eq!(t.polls, 3, "churn frames consumed, RSP matched");
    }

    // given the ONLY frame the link ever produces is a DATA packet,
    // when transact_strict collects, then it never accepts it as the
    // RSP and gives up at the bounded budget.
    #[test]
    fn transact_strict_times_out_without_ever_accepting_a_data_packet() {
        struct DataOnlyTransport {
            polls: usize,
        }
        impl Transport for DataOnlyTransport {
            fn send(&mut self, _cmd: &[u8]) -> bool {
                true
            }
            fn poll_frame(&mut self) -> Option<Frame> {
                self.polls += 1;
                Frame::decode(&[0x00, 0x00, 0x02, 0x90, 0x00])
            }
            fn transact(&mut self, _cmd: &[u8]) -> Option<Frame> {
                None
            }
            fn drain(&mut self) -> Option<Frame> {
                None
            }
        }
        let mut t = DataOnlyTransport { polls: 0 };
        assert_eq!(
            reader::transact_strict(&mut t, &reader::RF_DEACTIVATE_DISCOVERY),
            Err("no matching response")
        );
        assert_eq!(
            t.polls,
            reader::STRICT_RSP_POLLS,
            "bounded — no infinite churn loop"
        );
    }

    // given the teardown/resync leaves SEVERAL frames pending (late
    // DATA packet, WTX advisory, deactivate NTF, stale RSP — the Codex
    // PR159 P1 multi-frame residue), when drain_link runs, then every
    // frame is consumed until the link is actually empty — and a chatty
    // chip is capped at DRAIN_LINK_CAP.
    #[test]
    fn drain_link_empties_multi_frame_residue_bounded() {
        let mut t = mock::MockTransport::new();
        t.push_notification(&[0x00, 0x00, 0x02, 0x90, 0x00]); // late DATA packet
        t.push_notification(&[0x6F, 0x17, 0x00]); // WTX advisory
        t.push_notification(&[MT_NTF | GID_RF, NTF_RF_DEACTIVATE, 0x02, 0x00, 0x00]);
        t.push_notification(&[0x41, 0x03, 0x01, 0x00]); // stale RF_DISCOVER_RSP
        reader::drain_link(&mut t);
        assert_eq!(t.drained, 4, "every pending frame consumed");
        assert!(t.drain().is_none(), "link actually empty");

        let mut chatty = mock::MockTransport::new();
        for _ in 0..(reader::DRAIN_LINK_CAP + 4) {
            chatty.push_notification(&[0x6F, 0x17, 0x00]);
        }
        reader::drain_link(&mut chatty);
        assert_eq!(
            chatty.drained,
            reader::DRAIN_LINK_CAP,
            "cap bounds a chatty chip"
        );
    }

    #[test]
    fn settle_drain_consumes_frames_arriving_after_the_burst() {
        // #170: a WTX-honoring chip can deliver the timed-out
        // exchange's DATA response after the drain burst already found
        // the link empty — the plain burst stops at the first None and
        // the survivor answers the NEXT command's poll window
        // (#101-class desync, the wifi-vpcd zero-data wedge shape).
        // The settle beat (drain → wait → drain) catches it.
        let mut t = mock::MockTransport::new();
        t.late_on_wait.push_back(vec![0x00, 0x00, 0x02, 0x67, 0x00]); // the card's late 6700
        reader::settle_drain(&mut t);
        assert!(
            t.waited_ms >= reader::DATA_COLLECT_WAIT_MS,
            "a settle beat ran"
        );
        assert_eq!(
            t.drained, 1,
            "the post-settle burst consumed the late frame"
        );
        assert!(t.drain().is_none(), "link actually empty");
        assert!(
            t.poll_frame().is_none(),
            "nothing rides the next poll window"
        );
    }

    #[test]
    fn plain_drain_burst_misses_frames_arriving_after_it() {
        // The contrast that justifies settle_drain: drain_link alone
        // stops at the first empty poll and the late frame stays
        // queued for the next command to mis-read as its response.
        let mut t = mock::MockTransport::new();
        t.late_on_wait.push_back(vec![0x00, 0x00, 0x02, 0x67, 0x00]);
        reader::drain_link(&mut t);
        assert_eq!(t.drained, 0, "burst found nothing — frame not yet arrived");
        t.wait_ms(reader::DATA_COLLECT_WAIT_MS); // the chip delivers late
        let poisoned = t.poll_frame();
        assert!(
            poisoned.is_some(),
            "the hazard: a survivor rides the next command's window"
        );
    }

    #[test]
    fn data_fragment_headers_pbf_chain() {
        // NCI 1.0 §3.4.3 "Format of Data Packets": octet 0 =
        // MT(000b)|PBF(bit 4)|Conn ID(bits 3:0), octet 1 = RFU (0),
        // octet 2 = LEN. A 260-octet APDU (GP STORE DATA block) chunks
        // 253 + 7 at MAX_DATA_FRAGMENT; Table 3: "1b: The Packet
        // contains a segment of a Message which is not the last
        // segment" — only the 253-octet first fragment carries PBF.
        let mut apdu = [0u8; 260];
        for (i, b) in apdu.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let (head, tail) = apdu.split_at(data::MAX_DATA_FRAGMENT);
        assert_eq!((head.len(), tail.len()), (253, 7));

        let mut out = Vec::new();
        assert!(data::encode_fragment(0, true, head, &mut out));
        assert_eq!(out.len(), 3 + 253);
        assert_eq!(&out[..3], &[PBF_MASK, 0x00, 0xFD]);
        assert_eq!(&out[3..], head);

        assert!(data::encode_fragment(0, false, tail, &mut out));
        assert_eq!(out.len(), 3 + 7);
        assert_eq!(&out[..3], &[0x00, 0x00, 0x07]);
        assert_eq!(&out[3..], tail);

        // The historical single-packet path stays byte-identical to the
        // C driver (PBF clear) for payloads <= 255.
        let mut one = Vec::new();
        assert!(data::encode(0, &[0xAA, 0xBB], &mut one));
        assert_eq!(one.as_slice(), &[0x00, 0x00, 0x02, 0xAA, 0xBB]);
    }

    #[test]
    fn exchange_fragments_260_byte_apdu_into_pbf_chain() {
        // GP STORE DATA shape: 260-octet APDU → two DATA packets
        // (253 + 7), PBF=1 only on the first (NCI 1.0 §3.4.3/Table 3).
        // The credit re-grant (CORE_CONN_CREDITS_NTF `60 06 ..`,
        // §4.4.4; payload [num_entries=1, conn 0, +1 credit]) is
        // scripted to land between the fragments — without consuming it
        // the second send would overwrite the mock's pending reply and
        // the DATA response would never surface.
        let mut apdu = [0u8; 260];
        for (i, b) in apdu.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7);
        }
        let mut t = mock::MockTransport::new();
        t.push_reply(&[0x60, 0x06, 0x03, 0x01, 0x00, 0x01]); // credits NTF
        t.push_reply(&[0x00, 0x00, 0x02, 0x90, 0x00]); // DATA response
        let rsp = reader::exchange(&mut t, 0, &apdu).expect("fragmented exchange");
        assert_eq!(rsp.as_slice(), &[0x90, 0x00]);
        assert_eq!(t.sent.len(), 2, "exactly two DATA packets");
        let (f1, f2) = (&t.sent[0], &t.sent[1]);
        assert_eq!(f1.len(), 3 + 253);
        assert_eq!(&f1[..3], &[0x10, 0x00, 0xFD]); // PBF set, conn 0, LEN 253
        assert_eq!(&f1[3..], &apdu[..253]);
        assert_eq!(f2.len(), 3 + 7);
        assert_eq!(&f2[..3], &[0x00, 0x00, 0x07]); // PBF clear, LEN 7
        assert_eq!(&f2[3..], &apdu[253..]);
    }

    #[test]
    fn exchange_gates_second_fragment_on_credits_ntf() {
        // Ordering proof for the credit gate: the transport surfaces the
        // credits NTF only on the FIRST poll and the DATA response only
        // on the SECOND, recording the send/poll event order. NCI 1.0
        // §4.4.4: "The DH SHALL NOT send any Data Packet on a
        // connection when the corresponding NFCC_Credits_Avail is 0" —
        // on the PN7160 (1 credit/connection, UM11495 §5.3.1 Table 10)
        // fragment 1 consumes the only credit, so the event log must
        // read send → credits → send → data; a back-to-back
        // double-send would read send, send, credits, data.
        struct GatedTransport {
            sent: std::vec::Vec<std::vec::Vec<u8>>,
            events: std::vec::Vec<&'static str>,
            polls: usize,
        }
        impl Transport for GatedTransport {
            fn send(&mut self, cmd: &[u8]) -> bool {
                self.sent.push(cmd.to_vec());
                self.events.push("send");
                true
            }
            fn poll_frame(&mut self) -> Option<Frame> {
                self.polls += 1;
                match self.polls {
                    1 => {
                        self.events.push("credits");
                        Frame::decode(&[0x60, 0x06, 0x03, 0x01, 0x00, 0x01])
                    }
                    2 => {
                        self.events.push("data");
                        Frame::decode(&[0x00, 0x00, 0x02, 0x90, 0x00])
                    }
                    _ => None,
                }
            }
            fn transact(&mut self, _cmd: &[u8]) -> Option<Frame> {
                None
            }
            fn drain(&mut self) -> Option<Frame> {
                None
            }
        }
        let mut t = GatedTransport {
            sent: std::vec::Vec::new(),
            events: std::vec::Vec::new(),
            polls: 0,
        };
        let rsp = reader::exchange(&mut t, 0, &[0u8; 260]).expect("gated exchange");
        assert_eq!(rsp.as_slice(), &[0x90, 0x00]);
        assert_eq!(t.sent.len(), 2);
        assert_eq!(t.events, vec!["send", "credits", "send", "data"]);
    }

    #[test]
    fn exchange_reassembles_pbf_chained_data_response() {
        // NCI 1.0 §3.5: "When an NFCC sends a Data Message to the DH,
        // regardless of the payload length, it MAY segment the Data
        // Message into smaller Data Packets for any internal reason" —
        // a PBF=1 DATA packet followed by PBF=0 concatenates into one
        // response (the flag is Frame::pbf from decode).
        let mut t = mock::MockTransport::new();
        t.push_notification(&[0x10, 0x00, 0x02, 0xAA, 0xBB]); // segment, more follow
        t.push_notification(&[0x00, 0x00, 0x03, 0xCC, 0xDD, 0xEE]); // last segment
        let rsp =
            reader::exchange(&mut t, 0, &[0x00, 0xA4, 0x04, 0x00]).expect("reassembled response");
        assert_eq!(rsp.as_slice(), &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
    }

    #[test]
    fn exchange_253_byte_apdu_stays_single_packet() {
        // MAX_DATA_FRAGMENT boundary: a 253-octet APDU is exactly one
        // DATA packet with PBF clear and needs no credit wait — payloads
        // <= 253 keep the pre-fragmentation wire behavior.
        let mut t = mock::MockTransport::new();
        t.push_reply(&[0x00, 0x00, 0x02, 0x90, 0x00]);
        let rsp = reader::exchange(&mut t, 0, &[0xA5u8; 253]).expect("reply");
        assert_eq!(rsp.as_slice(), &[0x90, 0x00]);
        assert_eq!(t.sent.len(), 1);
        assert_eq!(t.sent[0].len(), 3 + 253);
        assert_eq!(&t.sent[0][..3], &[0x00, 0x00, 0xFD]);
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
