//! Thin CCID handler wrapper (docs/PERF_AND_DRY_STRATEGY.md §3.1):
//! command dispatch, slot lifecycle, escapes, parameters and the
//! APDU-level pipeline live in the shared, transport-agnostic
//! `ccid-core` handler ([`CcidHandlerCore`]); this wrapper keeps the
//! ESP32-specific T=1 endpoint routing (`t1.rs`, the libccidtwin TPDU
//! path) wired around it. Wire behavior is byte-identical to the
//! pre-extraction monolith (conformance-battery gate, #92 fuzz suite).

use crate::ccid_types::{
    build_bstatus, CcidHeader, SlotState, CCID_HEADER_SIZE, CMD_NOT_SUPPORTED,
    COMMAND_STATUS_FAILED, COMMAND_STATUS_NO_ERROR, ICC_NOT_ACTIVE, PC_TO_RDR_ESCAPE,
    PC_TO_RDR_XFR_BLOCK, RDR_TO_PC_DATABLOCK, RDR_TO_PC_ESCAPE,
};
use crate::nfc::{CardDriverAdapter, NfcDriver, PresenceState};
use crate::t1::{self, elapsed_ms_since, now_us, FeedResult, T1Endpoint, MAX_RELAY};
use crate::turnaround::TurnaroundStats;
use ccid_core::handler::CcidHandlerCore;
use ccid_core::response::{write_message, write_slot_status};
use ccid_core::Diagnostics;

// GEMPC: idVendor: 0x08E6 iManufacturer: Gemplus idProduct: 0x3437 iProduct: USB SmartCard Reader
const FIRMWARE_VERSION: &[u8] = b"GemPC Twin ESP32 1.0\0";

/// Card-timeout status word, the SW libccid synthesizes host-side for
/// bError 0xF0 (reference/CCID/src/commands.c CCID_Receive "case 0xF0").
/// ONE definition for every failure path that answers a dead card —
/// the T=1 INF synthesis and the vpcd APDU arm (Codex #203 P1).
pub const CARD_TIMEOUT_SW: [u8; 2] = [0x64, 0x00];

pub struct CcidHandler<D: NfcDriver> {
    core: CcidHandlerCore<CardDriverAdapter<D>>,
    t1: T1Endpoint,
    t1_armed: bool,
}

impl<D: NfcDriver> CcidHandler<D> {
    pub fn new(nfc: D) -> Self {
        Self {
            core: CcidHandlerCore::new(CardDriverAdapter::new(nfc), FIRMWARE_VERSION),
            t1: T1Endpoint::new(),
            t1_armed: false,
        }
    }

    /// Escape 0xD2 (issue #120): serve-loop stamp of the command
    /// turnaround (last command byte received → response flushed).
    pub fn record_turnaround_us(&mut self, us: u32) {
        self.core.driver_mut().1.record_turnaround_us(us);
    }

    /// Snapshot of the Escape 0xD2 counters.
    pub fn turnaround_stats(&self) -> TurnaroundStats {
        self.core.driver().1
    }

    pub fn take_snapshot_request(&mut self) -> bool {
        self.core.take_snapshot_request()
    }

    pub fn refresh_diagnostics(&mut self, uptime_ticks: u32) {
        self.core.refresh_diagnostics(uptime_ticks)
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        self.core.diagnostics()
    }

    pub fn driver_mut(&mut self) -> &mut D {
        &mut self.core.driver_mut().0
    }

    pub fn diagnostics_mut(&mut self) -> &mut Diagnostics {
        self.core.diagnostics_mut()
    }

    pub fn record_nak(&mut self) {
        self.core.record_nak()
    }

    pub fn check_card_change(&mut self) -> Option<bool> {
        self.core.check_card_change()
    }

    pub fn slot_state(&self) -> SlotState {
        self.core.slot_state()
    }

    pub fn current_protocol(&self) -> u8 {
        self.core.current_protocol()
    }

    pub fn set_current_protocol(&mut self, protocol: u8) {
        self.core.set_current_protocol(protocol)
    }

    pub fn presence_state(&self) -> PresenceState {
        self.core.presence_state()
    }

    pub fn sync_notifications(&self) -> bool {
        self.core.sync_notifications()
    }

    pub fn process_command(&mut self, ccid_msg: &[u8], response: &mut [u8]) -> usize {
        // libccidtwin T=1 wall fix (#101): at TPDU exchange level the
        // host's T=1 engine sends T=1 blocks (S-block IFSD requests,
        // I-blocks) as XfrBlock payloads. While SetParameters has armed
        // the route, they go through the local endpoint; the Relay path
        // re-enters the SAME shared APDU pipeline so unwrapped APDUs get
        // identical treatment. Everything else delegates to the core.
        if let Some(header) = CcidHeader::parse(ccid_msg) {
            if header.message_type == PC_TO_RDR_XFR_BLOCK
                && self.t1_armed
                && self.core.current_protocol() == 1
            {
                return self.xfr_via_t1(&header, ccid_msg, response);
            }
            // Escape 0xD2 (issue #120): turnaround counters, answered
            // here instead of the core's escape dispatch so the surface
            // stays local to the ESP32 serial profile (0xD0's 28-byte
            // core format is untouched).
            if header.message_type == PC_TO_RDR_ESCAPE
                && header.length == 1
                && ccid_msg.get(CCID_HEADER_SIZE) == Some(&0xD2)
            {
                let mut buf = [0u8; TurnaroundStats::SERIALIZED_SIZE];
                self.core.driver().1.to_bytes(&mut buf);
                return write_message(
                    RDR_TO_PC_ESCAPE,
                    header.slot,
                    header.seq,
                    build_bstatus(COMMAND_STATUS_NO_ERROR, self.core.slot_state().icc_status()),
                    0,
                    0,
                    &buf,
                    response,
                );
            }
            // Escape 0xD4 (issue #156 A3): PN7160 WTX advisory counters —
            // 8 bytes LE [last exchange, lifetime total]. The count of
            // 6F 17 00 RF_WTX notifications consumed during DATA
            // collection; bench model ≈ stall / 1.24 s (one advisory
            // per READER_FWITOX_NTF_CFG period, UM11495 Rev 1.8
            // Table 99 default 0x1000 × 4096/fc). Answered only on
            // backends that report WTX (PN7160) — elsewhere falls
            // through to the core's CMD_NOT_SUPPORTED.
            if header.message_type == PC_TO_RDR_ESCAPE
                && header.length == 1
                && ccid_msg.get(CCID_HEADER_SIZE) == Some(&0xD4)
            {
                if let Some((last, total)) = self.core.driver().0.wtx_counters() {
                    let payload = [last.to_le_bytes(), total.to_le_bytes()].concat();
                    return write_message(
                        RDR_TO_PC_ESCAPE,
                        header.slot,
                        header.seq,
                        build_bstatus(COMMAND_STATUS_NO_ERROR, self.core.slot_state().icc_status()),
                        0,
                        0,
                        &payload,
                        response,
                    );
                }
            }
            // Escape 0xD3 (issue #120): I2C poll-cadence probe — times N
            // back-to-back ComIrqReg reads (the exact transaction the
            // MFRC522 transceive poll loop performs) and reports
            // total_us, N, configured FWT. Payload out: 3 words LE.
            if header.message_type == PC_TO_RDR_ESCAPE
                && header.length == 2
                && ccid_msg.get(CCID_HEADER_SIZE) == Some(&0xD3)
            {
                let reads = u32::from(ccid_msg.get(CCID_HEADER_SIZE + 1).copied().unwrap_or(0));
                let (took_us, fwt_ms) = match self.core.driver_mut().0.i2c_timing_probe(reads) {
                    Some(us) => (us, self.core.driver().0.configured_fwt_ms()),
                    None => (u32::MAX, 0),
                };
                let payload = [
                    took_us.to_le_bytes(),
                    reads.to_le_bytes(),
                    fwt_ms.to_le_bytes(),
                ]
                .concat();
                let cmd_status = if took_us == u32::MAX {
                    COMMAND_STATUS_FAILED
                } else {
                    COMMAND_STATUS_NO_ERROR
                };
                let b_error = if took_us == u32::MAX {
                    CMD_NOT_SUPPORTED
                } else {
                    0
                };
                return write_message(
                    RDR_TO_PC_ESCAPE,
                    header.slot,
                    header.seq,
                    build_bstatus(cmd_status, self.core.slot_state().icc_status()),
                    b_error,
                    0,
                    &payload,
                    response,
                );
            }
        }

        let len = self.core.process_command(ccid_msg, response);

        if self.core.take_parameters_accepted() {
            self.t1_armed = true;
            // G-04: apply the negotiated T=1 timing to the endpoint —
            // WTX requests are then sized against the BWT the host
            // actually believes, and chain pieces up to the negotiated
            // IFSC are accepted.
            if let Some((bwi, _cwi, ifsc)) = self.core.applied_t1_timing() {
                self.t1.set_host_timing(bwi, ifsc);
            }
        }
        if self.core.take_card_reset() {
            // Card reset invalidates the negotiated protocol (CCID
            // spec §6.1.2: parameters do not survive ATR reset) — disarm
            // the T=1 route so pcscd's post-reset PPS probe (`FF 01 FE`,
            // sent BEFORE SetParameters) echoes as PPS instead of
            // feeding a stale T=1 endpoint (which parses it as garbage
            // and kills the session — bench-strace 2026-10-10: every
            // second connect failed "Card is unresponsive" exactly
            // here). SetParameters re-arms.
            self.t1.reset();
            self.t1_armed = false;
        }

        len
    }

    fn xfr_via_t1(&mut self, header: &CcidHeader, ccid_msg: &[u8], response: &mut [u8]) -> usize {
        let payload_len = header.length as usize;
        if ccid_msg.len() < CCID_HEADER_SIZE + payload_len {
            // Same direct-input truncation guard + single error count as
            // the core dispatcher (issue #114, Codex on #93): the early
            // return bypasses the post-dispatch status accounting.
            self.core.record_protocol_error();
            return write_slot_status(
                header.slot,
                header.seq,
                self.core.slot_state().icc_status(),
                COMMAND_STATUS_FAILED,
                CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }
        let apdu = &ccid_msg[CCID_HEADER_SIZE..CCID_HEADER_SIZE + payload_len];

        // ISO 7816-3 §11.5.2 (issue #101 design note): S-block requests
        // (RESYNC/IFS/ABORT/WTX negotiation) are the READER's own
        // link-layer duty — answer them even without an active card
        // session, so battery/verification tooling can probe T=1
        // liveness on an empty slot. pcscd is unaffected (it negotiates
        // T=1 only after connect). I-blocks still need the card below.
        if t1::is_s_block_request(apdu) {
            if let FeedResult::Immediate(bytes) = self.t1.feed(apdu) {
                let len = write_message(
                    RDR_TO_PC_DATABLOCK,
                    header.slot,
                    header.seq,
                    build_bstatus(COMMAND_STATUS_NO_ERROR, self.core.slot_state().icc_status()),
                    0,
                    0,
                    &bytes,
                    response,
                );
                return self.account_failed_status(len, response);
            }
        }

        if self.core.slot_state() != SlotState::PresentActive {
            let len = write_message(
                RDR_TO_PC_DATABLOCK,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_FAILED, self.core.slot_state().icc_status()),
                ICC_NOT_ACTIVE,
                0,
                &[],
                response,
            );
            return self.account_failed_status(len, response);
        }

        match self.t1.feed(apdu) {
            FeedResult::Immediate(bytes) => write_message(
                RDR_TO_PC_DATABLOCK,
                header.slot,
                header.seq,
                build_bstatus(
                    COMMAND_STATUS_NO_ERROR,
                    SlotState::PresentActive.icc_status(),
                ),
                0,
                0,
                &bytes,
                response,
            ),
            FeedResult::Apdu(card_apdu) => {
                // G-03: GP STORE DATA (CAP install) chains case-3/4
                // APDUs past 254 — the old min() clamp at 254 corrupted
                // them while reporting success. Sized from t1::MAX_RELAY
                // (the assembler's Vec capacity), so truncation here is
                // impossible by construction.
                let mut apdu_buf = [0u8; MAX_RELAY];
                apdu_buf[..card_apdu.len()].copy_from_slice(&card_apdu);
                let an = card_apdu.len();
                let mut scratch = [0u8; 271];
                let started_us = now_us();
                let n = self
                    .core
                    .apdu_pipeline(header, &apdu_buf[..an], &mut scratch);
                let took = elapsed_ms_since(started_us);
                // #152: at TPDU exchange level the host runs its own
                // T=1 engine (libccid proto-t1.c t1_xcv → CCID_Receive)
                // and only ever sees the I-block INF we frame here — a
                // DataBlock bStatus never reaches the application. When
                // the card dies mid-exchange the pipeline answers a
                // FAILED DataBlock whose payload is empty; framing that
                // empty payload as a SUCCESS I-block (LEN=0) delivers
                // SUCCESS + 0 bytes to the app — the GlobalPlatformPro
                // ByteBuffer crash (gp #307, "newPosition < 0: -2").
                // Synthesize the card-timeout SW 64 00 as the INF: the
                // same SW libccid itself synthesizes for bError 0xF0
                // (commands.c CCID_Receive, "case 0xF0") — one
                // convention for both serial sub-paths.
                let payload: &[u8] = if scratch[7] & (COMMAND_STATUS_FAILED << 6) != 0 {
                    &CARD_TIMEOUT_SW
                } else {
                    // The pipeline returns a full DataBlock message;
                    // T=1 INF carries only the APDU-level payload —
                    // strip the 10-byte CCID header.
                    &scratch[10.min(n)..n]
                };
                let blocks = self.t1.card_response(payload, took);
                write_message(
                    RDR_TO_PC_DATABLOCK,
                    header.slot,
                    header.seq,
                    build_bstatus(
                        COMMAND_STATUS_NO_ERROR,
                        SlotState::PresentActive.icc_status(),
                    ),
                    0,
                    0,
                    &blocks,
                    response,
                )
            }
        }
    }

    /// Post-dispatch status accounting, identical to the core's: a
    /// failed bStatus in the written response counts one protocol error.
    fn account_failed_status(&mut self, len: usize, response: &[u8]) -> usize {
        if len > 7 && response[7] & 0x40 != 0 {
            self.core.record_protocol_error();
        }
        len
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ccid_types::{
        DEFAULT_T0_PARAMS, DEFAULT_T1_PARAMS, ICC_STATUS_NO_ICC, PC_TO_RDR_ESCAPE,
        PC_TO_RDR_GET_PARAMETERS, PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_POWER_OFF,
        PC_TO_RDR_ICC_POWER_ON, PC_TO_RDR_RESET_PARAMETERS, PC_TO_RDR_SET_PARAMETERS,
        RDR_TO_PC_ESCAPE, RDR_TO_PC_PARAMETERS, RDR_TO_PC_SLOTSTATUS,
    };
    use crate::nfc::MockNfcDriver;
    use std::vec::Vec;

    const ATR: [u8; 5] = [0x3B, 0x80, 0x80, 0x01, 0x01];
    const APDU_RESPONSE: [u8; 2] = [0x90, 0x00];

    fn build_ccid_cmd(msg_type: u8, slot: u8, seq: u8, payload: &[u8]) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.push(msg_type);
        let len = payload.len() as u32;
        msg.extend_from_slice(&len.to_le_bytes());
        msg.push(slot);
        msg.push(seq);
        msg.push(0x00);
        msg.push(0x00);
        msg.push(0x00);
        msg.extend_from_slice(payload);
        msg
    }

    fn build_set_parameters_cmd(slot: u8, seq: u8, protocol: u8, payload: &[u8]) -> Vec<u8> {
        let mut msg = build_ccid_cmd(PC_TO_RDR_SET_PARAMETERS, slot, seq, payload);
        msg[7] = protocol;
        msg
    }

    fn parse_response(bytes: &[u8]) -> (CcidHeader, &[u8]) {
        let header = CcidHeader::parse(bytes).expect("response header");
        let payload_len = header.length as usize;
        (
            header.clone(),
            &bytes[CCID_HEADER_SIZE..CCID_HEADER_SIZE + payload_len],
        )
    }

    fn new_handler(card_present: bool) -> CcidHandler<MockNfcDriver> {
        CcidHandler::new(MockNfcDriver::new(card_present, &ATR, &APDU_RESPONSE))
    }

    #[test]
    fn test_power_on_with_card_returns_atr() {
        let mut handler = new_handler(true);
        handler.check_card_change(); // simulate card detection poll
        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 7, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(header.slot, 0);
        assert_eq!(header.seq, 7);
        assert_eq!(
            header.specific,
            [
                build_bstatus(
                    COMMAND_STATUS_NO_ERROR,
                    SlotState::PresentActive.icc_status()
                ),
                0,
                0
            ]
        );
        assert_eq!(payload, ATR);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);
    }

    #[test]
    fn test_power_on_without_card_returns_slot_status_error() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 1, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC)
        );
        assert_eq!(header.specific[1], CMD_NOT_SUPPORTED);
        assert_eq!(handler.slot_state(), SlotState::Absent);
    }

    #[test]
    fn test_power_off_returns_present_inactive() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 2, &[]);
        handler.process_command(&power_on, &mut response);

        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_OFF, 0, 3, &[]);
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
    }

    #[test]
    fn test_get_slot_status_with_card_reports_present() {
        let mut handler = new_handler(true);
        // Card presence not known until first poll
        handler.check_card_change();
        let cmd = build_ccid_cmd(PC_TO_RDR_GET_SLOT_STATUS, 0, 4, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, _) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
    }

    #[test]
    fn test_get_slot_status_without_card_reports_not_present() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_GET_SLOT_STATUS, 0, 5, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, _) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_NO_ICC)
        );
        assert_eq!(handler.slot_state(), SlotState::Absent);
    }

    #[test]
    fn test_xfr_block_succeeds_when_card_is_active() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 7, &[0x00, 0xA4, 0x04, 0x00, 0x00]);
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentActive.icc_status()
            )
        );
        assert_eq!(payload, APDU_RESPONSE);
    }

    #[test]
    fn test_xfr_block_when_not_powered_returns_icc_not_active() {
        let mut handler = new_handler(true);
        // Poll so handler knows card is present (but not powered)
        handler.check_card_change();
        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 8, &[0x00, 0x84, 0x00, 0x00]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_FAILED,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(header.specific[1], ICC_NOT_ACTIVE);
    }

    #[test]
    fn test_escape_firmware_returns_version_string() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 9, &[0x02]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_NO_ICC)
        );
        assert_eq!(payload, FIRMWARE_VERSION);
    }

    #[test]
    fn test_escape_snapshot_arms_exactly_once() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 9, &[0xD1]);
        let mut response = [0u8; 271];

        assert!(!handler.take_snapshot_request());

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert_eq!(payload, &[0xD1]);
        // The serve loop consumes the flag AFTER the ack went out; it must
        // fire exactly once so a retried 0xD1 is a fresh snapshot.
        assert!(handler.take_snapshot_request());
        assert!(!handler.take_snapshot_request());
    }

    #[test]
    fn escape_0xd2_returns_recorded_turnaround_bytes() {
        let mut handler = new_handler(true);
        handler.record_turnaround_us(3_200_000);
        handler.record_turnaround_us(2_100_000);
        handler.record_turnaround_us(1_500_000);

        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 40, &[0xD2]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert_eq!(header.specific[0] & 0xC0, 0x00, "command status OK");
        assert_eq!(payload.len(), TurnaroundStats::SERIALIZED_SIZE);
        let mut expected = [0u8; TurnaroundStats::SERIALIZED_SIZE];
        handler.turnaround_stats().to_bytes(&mut expected);
        assert_eq!(payload, expected);
        // last=1_500_000, max=3_200_000 in the first two LE words
        assert_eq!(
            u32::from_le_bytes(payload[0..4].try_into().unwrap()),
            1_500_000
        );
        assert_eq!(
            u32::from_le_bytes(payload[4..8].try_into().unwrap()),
            3_200_000
        );
    }

    #[test]
    fn escape_0xd2_zeroed_before_any_recording() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 41, &[0xD2]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert_eq!(payload.len(), TurnaroundStats::SERIALIZED_SIZE);
        assert!(payload.iter().all(|&b| b == 0));
    }

    #[test]
    fn escape_0xd2_only_matches_single_byte_payload() {
        // A longer escape carrying 0xD2 first must NOT hit the counters —
        // it falls through to the core dispatch, which rejects it as an
        // Escape-typed CMD_NOT_SUPPORTED failure (#141: unsupported escape
        // payloads answer RDR_to_PC_Escape, matching osmo + the STM32
        // sibling), not a SlotStatus frame.
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 42, &[0xD2, 0x00]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, _) = parse_response(&response[..len]);

        assert_eq!(
            header.message_type,
            crate::ccid_types::RDR_TO_PC_ESCAPE,
            "multi-byte escape is not the 0xD2 query"
        );
        assert_eq!(header.specific[1], CMD_NOT_SUPPORTED);
    }

    #[test]
    fn escape_0xd4_returns_wtx_counters_le() {
        // Issue #156 A3: 8 bytes LE [last exchange, lifetime total].
        let mut handler = new_handler(true);
        handler.driver_mut().set_wtx_counters(2, 5);

        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 43, &[0xD4]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert_eq!(header.specific[0] & 0xC0, 0x00, "command status OK");
        assert_eq!(payload.len(), 8);
        assert_eq!(u32::from_le_bytes(payload[0..4].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(payload[4..8].try_into().unwrap()), 5);
    }

    #[test]
    fn escape_0xd4_without_wtx_backend_falls_through() {
        // MFRC522-class backends report no WTX counters — the escape
        // is not theirs to answer, so the core dispatch rejects it as
        // an Escape-typed CMD_NOT_SUPPORTED failure (#141 host
        // contract: unsupported escape payloads answer
        // RDR_to_PC_Escape, matching osmo + the STM32 sibling), not a
        // SlotStatus frame.
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 44, &[0xD4]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, _) = parse_response(&response[..len]);

        assert_eq!(
            header.message_type,
            crate::ccid_types::RDR_TO_PC_ESCAPE,
            "no WTX backend: core's Escape-typed rejection"
        );
        assert_eq!(header.specific[1], CMD_NOT_SUPPORTED);
    }

    #[test]
    fn card_exchange_recorded_through_apdu_pipeline() {
        // Host now_us() is a stub, so the adapter records a zero sample —
        // but the recording must happen for EVERY transmit_apdu, which we
        // observe through the stats snapshot staying consistent (a
        // non-stamp would leave the counters at their previous value
        // through a power-on + APDU exchange).
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);
        handler.record_turnaround_us(999);
        let before = handler.turnaround_stats();

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 43, &[0x00, 0xA4, 0x04, 0x00, 0x00]);
        let mut response = [0u8; 271];
        handler.process_command(&cmd, &mut response);

        let after = handler.turnaround_stats();
        assert_eq!(after.last_turnaround_us(), 999, "serve stamp untouched");
        assert_eq!(
            after.last_card_exchange_us(),
            before.last_card_exchange_us(),
            "host clock stub records zero"
        );
    }

    #[test]
    fn test_escape_sync_notification_enables_sync_mode() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 10, &[0x01, 0x01, 0x01]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert_eq!(payload, [0x01, 0x01, 0x01]);
        assert!(handler.sync_notifications());
    }

    #[test]
    fn test_unknown_command_returns_cmd_not_supported() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(0x71, 0, 11, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC)
        );
        assert_eq!(header.specific[1], CMD_NOT_SUPPORTED);
    }

    #[test]
    fn test_check_card_change_detects_insertion_and_removal() {
        let mut handler = new_handler(false);

        assert_eq!(handler.check_card_change(), None);

        handler.driver_mut().set_card_present(true);
        assert_eq!(handler.check_card_change(), Some(true));

        // full activation, then removal
        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 30, &[]);
        handler.process_command(&power_on, &mut response);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);

        handler.driver_mut().set_card_present(false);
        assert_eq!(handler.check_card_change(), Some(false));
        assert_eq!(handler.slot_state(), SlotState::Absent);
    }

    #[test]
    fn test_session_lifecycle() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 16, &[]);
        let power_on_len = handler.process_command(&power_on, &mut response);
        let (power_on_header, power_on_payload) = parse_response(&response[..power_on_len]);

        assert_eq!(power_on_header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(power_on_payload, ATR);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);
        assert!(handler.driver_mut().session_active());

        let xfr = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 17, &[0x00, 0x84, 0x00, 0x00]);
        let xfr_len = handler.process_command(&xfr, &mut response);
        let (xfr_header, xfr_payload) = parse_response(&response[..xfr_len]);

        assert_eq!(xfr_header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(xfr_payload, APDU_RESPONSE);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);
        assert!(handler.driver_mut().session_active());

        let power_off = build_ccid_cmd(PC_TO_RDR_ICC_POWER_OFF, 0, 18, &[]);
        let power_off_len = handler.process_command(&power_off, &mut response);
        let (power_off_header, power_off_payload) = parse_response(&response[..power_off_len]);

        assert_eq!(power_off_header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert!(power_off_payload.is_empty());
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
        assert!(!handler.driver_mut().session_active());
    }

    #[test]
    fn test_poll_skips_when_session_active() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);

        assert_eq!(handler.check_card_change(), None);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);
        assert!(handler.presence_state().present);
    }

    #[test]
    fn test_apdu_failure_downgrades_to_present_inactive_when_card_remains_present() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);
        handler.driver_mut().power_off();

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 7, &[0x00, 0xA4, 0x04, 0x00]);
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_FAILED,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
    }

    #[test]
    fn test_get_parameters_returns_default_t1_params() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let cmd = build_ccid_cmd(PC_TO_RDR_GET_PARAMETERS, 0, 12, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_PARAMETERS);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(header.specific[2], 1);
        assert_eq!(payload, DEFAULT_T1_PARAMS);
    }

    #[test]
    fn test_set_parameters_updates_protocol_and_returns_t0_params() {
        let mut handler = new_handler(true);
        let cmd = build_set_parameters_cmd(0, 13, 0, &DEFAULT_T0_PARAMS);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_PARAMETERS);
        assert_eq!(header.specific[2], 0);
        assert_eq!(payload, DEFAULT_T0_PARAMS);
        assert_eq!(handler.current_protocol(), 0);
    }

    #[test]
    fn test_set_parameters_rejects_invalid_protocol_num() {
        // CCID Rev 1.1 §6.1.7: bProtocolNum is 0 (T=0) or 1 (T=1);
        // anything else must be rejected, not stored and echoed.
        let mut handler = new_handler(true);
        let cmd = build_set_parameters_cmd(0, 21, 2, &DEFAULT_T1_PARAMS);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(header.seq, 21);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC)
        );
        assert_eq!(header.specific[1], CMD_NOT_SUPPORTED);
        assert_eq!(handler.current_protocol(), 1);
        assert!(!handler.t1_armed);
    }

    #[test]
    fn test_reset_parameters_restores_t1_defaults() {
        let mut handler = new_handler(true);
        let set_cmd = build_set_parameters_cmd(0, 14, 0, &DEFAULT_T0_PARAMS);
        let mut response = [0u8; 271];
        handler.process_command(&set_cmd, &mut response);

        let reset_cmd = build_ccid_cmd(PC_TO_RDR_RESET_PARAMETERS, 0, 15, &[]);
        let len = handler.process_command(&reset_cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_PARAMETERS);
        assert_eq!(header.specific[2], 1);
        assert_eq!(payload, DEFAULT_T1_PARAMS);
        assert_eq!(handler.current_protocol(), 1);
    }

    #[test]
    fn test_xfr_block_increments_apdu_tx_count() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);

        assert_eq!(handler.diagnostics().apdu_tx_count, 0);
        assert_eq!(handler.diagnostics().apdu_rx_count, 0);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 7, &[0x00, 0xA4, 0x04, 0x00]);
        handler.process_command(&cmd, &mut response);

        assert_eq!(handler.diagnostics().apdu_tx_count, 1);
        assert_eq!(handler.diagnostics().apdu_rx_count, 1);
    }

    #[test]
    fn test_error_response_increments_error_count() {
        let mut handler = new_handler(false);
        let mut response = [0u8; 271];

        assert_eq!(handler.diagnostics().error_count, 0);

        let cmd = build_ccid_cmd(0x71, 0, 11, &[]);
        handler.process_command(&cmd, &mut response);

        assert_eq!(handler.diagnostics().error_count, 1);
    }

    #[test]
    fn test_card_present_reflected_in_diagnostics() {
        let mut handler = new_handler(false);
        handler.check_card_change();
        assert!(!handler.diagnostics().card_present);

        handler.driver_mut().set_card_present(true);
        handler.check_card_change();
        assert!(handler.diagnostics().card_present);

        handler.driver_mut().set_card_present(false);
        handler.check_card_change();
        assert!(!handler.diagnostics().card_present);
    }

    const CARD_UID: [u8; 7] = [0x04, 0xC4, 0x74, 0xFA, 0x96, 0x73, 0x80];

    fn new_handler_with_uid(card_present: bool) -> CcidHandler<MockNfcDriver> {
        let mut nfc = MockNfcDriver::new(card_present, &ATR, &APDU_RESPONSE);
        nfc.set_uid(&CARD_UID);
        CcidHandler::new(nfc)
    }

    fn power_on(handler: &mut CcidHandler<MockNfcDriver>) {
        handler.check_card_change();
        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 30, &[]);
        let mut response = [0u8; 271];
        handler.process_command(&cmd, &mut response);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);
    }

    #[test]
    fn test_get_uid_pseudo_apdu_returns_uid_and_9000() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 31, &[0xFF, 0xCA, 0x00, 0x00, 0x00]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentActive.icc_status()
            )
        );
        let mut expected = CARD_UID.to_vec();
        expected.extend_from_slice(&[0x90, 0x00]);
        assert_eq!(payload, expected.as_slice());
    }

    #[test]
    fn test_get_uid_bad_p1_p2_returns_6a86() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 32, &[0xFF, 0xCA, 0x01, 0x00, 0x00]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (_, payload) = parse_response(&response[..len]);

        assert_eq!(payload, [0x6A, 0x86]);
    }

    #[test]
    fn test_ff_cla_unsupported_ins_returns_6300() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);

        // malformed_invalid_cla from the difftest matrix: the mock would
        // answer APDU_RESPONSE (9000) if this reached transmit_apdu.
        let cmd = build_ccid_cmd(
            PC_TO_RDR_XFR_BLOCK,
            0,
            33,
            &[0xFF, 0xA4, 0x00, 0x0C, 0x02, 0x3F, 0x00],
        );
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (_, payload) = parse_response(&response[..len]);

        assert_eq!(payload, [0x63, 0x00]);
    }

    #[test]
    fn test_short_ff_fragment_still_forwarded_to_card() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 34, &[0xFF]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (_, payload) = parse_response(&response[..len]);

        assert_eq!(payload, APDU_RESPONSE);
    }

    #[test]
    fn test_ff_pps_request_not_intercepted_as_pseudo_apdu() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);

        let pps = [0xFF, 0x11, 0x00];
        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 35, &pps);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (_, payload) = parse_response(&response[..len]);

        assert_eq!(payload, pps);
    }

    #[test]
    fn test_get_uid_without_cached_uid_returns_6300() {
        let mut handler = new_handler(true);
        power_on(&mut handler);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 36, &[0xFF, 0xCA, 0x00, 0x00, 0x00]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (_, payload) = parse_response(&response[..len]);

        assert_eq!(payload, [0x63, 0x00]);
    }

    #[test]
    fn test_get_uid_requires_active_slot() {
        let mut handler = new_handler_with_uid(true);
        handler.check_card_change();

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 37, &[0xFF, 0xCA, 0x00, 0x00, 0x00]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert!(payload.is_empty());
        assert_eq!(header.specific[1], ICC_NOT_ACTIVE);
    }

    #[test]
    fn power_on_disarms_stale_t1_route_for_pps_probe() {
        // Regression (bench-strace 2026-10-10): pcscd's connect sequence
        // sends a PPS probe (FF 01 FE) BEFORE SetParameters. If a prior
        // session left t1_armed set, the PPS feeds the T=1 endpoint and
        // the R-block garbage answer kills every subsequent connect.
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);
        handler.t1_armed = true; // stale from a previous session
                                 // simulate the card reset of a NEW connect: power cycle
        let off = build_ccid_cmd(0x63, 0, 0, &[]);
        let mut response = [0u8; 271];
        let _ = handler.process_command(&off, &mut response);
        power_on(&mut handler);

        // After the card reset the route must be disarmed: a PPS request
        // echoes as PPS (direct path), not as a T=1 R-block.
        let pps = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 1, &[0xFF, 0x01, 0xFE]);
        let len = handler.process_command(&pps, &mut response);
        let (header, payload) = parse_response(&response[..len]);
        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(
            payload,
            &[0xFF, 0x01, 0xFE],
            "PPS must echo before SetParameters"
        );
    }

    #[test]
    fn s_block_answered_without_active_card() {
        // Issue #101 design note: S-block negotiation is the reader's own
        // link-layer duty (ISO 7816-3 s11.5.2) — the T=1 route must answer
        // an IFS request even when the slot has NO active card session
        // (battery/verification liveness probes), while I-blocks stay
        // gated on the card.
        let mut handler = new_handler(false); // card NOT present
        handler.t1_armed = true;
        handler.set_current_protocol(1);

        let ifsd = [0x00u8, 0xC1, 0x01, 0xFE, 0x3E];
        let msg = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 1, &ifsd);
        let mut resp = [0u8; 512];
        let n = handler.process_command(&msg, &mut resp);
        let (header, payload) = parse_response(&resp[..n]);
        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(
            payload,
            &[0x00, 0xE1, 0x01, 0xFE, 0x1E][..],
            "S-IFS must answer byte-exact with no card"
        );
        // command status OK despite inactive ICC
        assert_eq!(header.specific[0] & 0xC0, 0x00, "cmd status must be OK");

        // I-blocks still require the card:
        let iblock = [0x00u8, 0x02, 0x05, 0x00, 0xA4, 0x04, 0x00, 0x00, 0x00];
        let msg = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 1, &iblock);
        let n = handler.process_command(&msg, &mut resp);
        let (header, payload) = parse_response(&resp[..n]);
        assert!(
            header.specific[0] & 0xC0 != 0 || payload.is_empty(),
            "I-block without card must fail"
        );
    }

    #[test]
    fn t1_ifsd_request_answered_on_wire() {
        // The exact libccidtwin exchange (bench-proven bytes): the host
        // sends S-block IFS request 00 C1 01 FE LRC as an XfrBlock
        // payload; the reader must answer 00 E1 01 FE LRC — proto-t1.c
        // lines 795-810 reject anything else.
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);
        handler.set_current_protocol(1);
        handler.t1_armed = true;
        let ifsd = [0x00u8, 0xC1, 0x01, 0xFE, 0x3E];
        let msg = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 1, &ifsd);
        let mut resp = [0u8; 512];
        let n = handler.process_command(&msg, &mut resp);
        let (_hdr, payload) = parse_response(&resp[..n]);
        assert_eq!(
            payload,
            &[0x00, 0xE1, 0x01, 0xFE, 0x1E][..],
            "S-block IFS response must be byte-exact"
        );
    }

    #[test]
    fn t1_i_block_apdu_relays_and_returns_i_block() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);
        handler.set_current_protocol(1);
        handler.t1_armed = true;
        let mut resp = [0u8; 512];

        let apdu = [0x00u8, 0xA4, 0x04, 0x00, 0x00];
        let mut iblock = [0x00u8, 0x00, apdu.len() as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        iblock[3..3 + apdu.len()].copy_from_slice(&apdu);
        let end = 3 + apdu.len();
        iblock[end] = iblock[..end].iter().fold(0u8, |a, b| a ^ b);

        let msg = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 2, &iblock[..end + 1]);
        let n = handler.process_command(&msg, &mut resp);
        assert!(n >= 10);
        let (_hdr, payload) = parse_response(&resp[..n]);
        assert_eq!(payload[0], 0x00);
        assert_eq!(payload[1] & 0x80, 0x00, "I-block, not R/S");
        let plen = payload[2] as usize;
        assert_eq!(&payload[3..3 + plen], &[0x90, 0x00], "card answer in INF");
        let e = 3 + plen;
        assert_eq!(payload[e], payload[..e].iter().fold(0u8, |a, b| a ^ b));
    }

    #[test]
    fn chained_apdu_over_254_reaches_card_untruncated() {
        // G-03 regression: GP STORE DATA during CAP install chains a
        // case-3/4 APDU past 254 bytes; the relay used to clamp it to
        // 254 silently while reporting success.
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);
        handler.set_current_protocol(1);
        handler.t1_armed = true;
        let mut resp = [0u8; 512];

        let apdu: Vec<u8> = std::iter::once(0x80)
            .chain(std::iter::repeat(0x5A).take(298))
            .chain([0x90, 0x00].iter().copied())
            .collect();
        assert_eq!(apdu.len(), 301);

        let chunks: Vec<&[u8]> = apdu.chunks(32).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let more = i < chunks.len() - 1;
            let pcb = (0u8 << 6) | ((more as u8) << 5);
            let mut blk = vec![0x00u8, pcb, chunk.len() as u8];
            blk.extend_from_slice(chunk);
            blk.push(blk.iter().fold(0u8, |a, b| a ^ b));

            let msg = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 2, &blk);
            let n = handler.process_command(&msg, &mut resp);
            assert!(n >= 10);
            let (_hdr, payload) = parse_response(&resp[..n]);
            if more {
                assert_eq!(payload[1], 0x80, "chain piece ACKed with R(0, 0)");
            } else {
                let plen = payload[2] as usize;
                assert_eq!(&payload[3..3 + plen], &[0x90, 0x00]);
            }
        }

        assert_eq!(
            handler.core.driver_mut().0.last_apdu(),
            apdu.as_slice(),
            "full 301-byte APDU must reach the card — G-03 truncation"
        );
    }

    #[test]
    fn set_parameters_applies_t1_timing_to_endpoint() {
        // G-04: the negotiated BWI/IFSC must reach the endpoint, and
        // GetParameters must echo the accepted set (what pcscd believes)
        // instead of the static defaults.
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);
        handler.set_current_protocol(1);
        handler.t1_armed = true;
        let mut resp = [0u8; 512];

        let mut params = DEFAULT_T1_PARAMS;
        params[5] = 0xFE; // bIFSC 254 — what the host derives from our TA3
        let cmd = build_set_parameters_cmd(0, 9, 1, &params);
        let n = handler.process_command(&cmd, &mut resp);
        let (header, payload) = parse_response(&resp[..n]);
        assert_eq!(header.message_type, RDR_TO_PC_PARAMETERS);
        assert_eq!(payload, params, "SetParameters must echo the accepted set");

        // A 64-byte INF piece exceeds DEFAULT_HOST_IFSD (32) but fits
        // the negotiated 254 — must be chain-ACKed, not bad_block'ed.
        let piece = [0x5Au8; 64];
        let pcb = (0u8 << 6) | (1u8 << 5);
        let mut blk = vec![0x00u8, pcb, piece.len() as u8];
        blk.extend_from_slice(&piece);
        blk.push(blk.iter().fold(0u8, |a, b| a ^ b));
        let msg = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 10, &blk);
        let n = handler.process_command(&msg, &mut resp);
        let (_hdr, payload) = parse_response(&resp[..n]);
        assert_eq!(payload[1], 0x80, "chain piece within negotiated IFSC ACKed");
    }

    #[test]
    fn malformed_block_under_t1_demands_retransmit() {
        // At TPDU level the host engine sends ONLY T=1 blocks; a 4-byte
        // fragment is a malformed block (LEN says 4, only 1 INF byte
        // arrived) — the endpoint answers R(N(R), error) per §11.5.3.
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);
        handler.set_current_protocol(1);
        handler.t1_armed = true;
        let mut resp = [0u8; 512];

        let fragment = [0x00u8, 0xA4, 0x04, 0x00];
        let msg = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 3, &fragment);
        let n = handler.process_command(&msg, &mut resp);
        assert!(n >= 10);
        let (_hdr, payload) = parse_response(&resp[..n]);
        assert_eq!(payload[1] & 0xC0, 0x80, "R-block, not I/S");
    }

    #[test]
    fn retransmitted_t1_iblock_serves_cache_not_card() {
        // #101: a retransmitted I-block XfrBlock (our DataBlock was
        // lost on the wire) must return byte-identical bytes with NO
        // second card exchange — re-executing an INSTALL-class APDU is
        // the double-INSTALL hazard.
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);
        handler.set_current_protocol(1);
        handler.t1_armed = true;
        let mut resp = [0u8; 512];

        let apdu = [0x84u8, 0xE6, 0x02, 0x00, 0x00];
        let mut iblock = [0u8; 3 + 5 + 1];
        iblock[0] = 0x00;
        iblock[1] = 0x00;
        iblock[2] = apdu.len() as u8;
        iblock[3..3 + apdu.len()].copy_from_slice(&apdu);
        let end = 3 + apdu.len();
        iblock[end] = iblock[..end].iter().fold(0u8, |a, b| a ^ b);

        let msg = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 2, &iblock[..end + 1]);
        let n = handler.process_command(&msg, &mut resp);
        let (_hdr, first_payload) = parse_response(&resp[..n]);
        let first_payload = first_payload.to_vec();
        let tx_after_first = handler.diagnostics().apdu_tx_count;

        let retry = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 3, &iblock[..end + 1]);
        let n2 = handler.process_command(&retry, &mut resp);
        let (_hdr2, retry_payload) = parse_response(&resp[..n2]);

        assert_eq!(
            first_payload, retry_payload,
            "replay must be byte-identical"
        );
        assert_eq!(
            handler.diagnostics().apdu_tx_count,
            tx_after_first,
            "no second card exchange"
        );
    }

    // GREAT EXPECTATION (#152): card death mid-exchange under an armed
    // T=1 route must answer with a synthesized card-timeout SW in the
    // I-block INF — NOT an empty INF. At TPDU level the host's own T=1
    // engine (libccid proto-t1.c t1_xcv) sees only the INF; an empty
    // INF delivers SUCCESS + 0 bytes to the app — the GlobalPlatformPro
    // ByteBuffer crash (gp #307). SW 64 00 = the same SW libccid
    // synthesizes for bError 0xF0 (commands.c CCID_Receive "case 0xF0").
    #[test]
    fn t1_card_death_mid_exchange_answers_synthesized_timeout_sw() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);
        handler.set_current_protocol(1);
        handler.t1_armed = true;
        // Card dies mid-session: transmit_apdu now fails (mock: NoCard).
        handler.core.driver_mut().0.set_card_present(false);
        let mut resp = [0u8; 512];

        let apdu = [0x00u8, 0xA4, 0x04, 0x00, 0x00];
        let mut iblock = [0u8; 3 + 5 + 1];
        iblock[0] = 0x00;
        iblock[1] = 0x00;
        iblock[2] = apdu.len() as u8;
        iblock[3..3 + apdu.len()].copy_from_slice(&apdu);
        let end = 3 + apdu.len();
        iblock[end] = iblock[..end].iter().fold(0u8, |a, b| a ^ b);

        let msg = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 2, &iblock[..end + 1]);
        let n = handler.process_command(&msg, &mut resp);
        let (header, payload) = parse_response(&resp[..n]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(header.specific[1], 0);
        // T=1 block = NAD + PCB + LEN + INF + LRC: I-block (PCB bit7=0)
        // whose INF is the synthesized SW: LEN=2, 64 00, valid LRC —
        // the app sees SW 6400, never empty data.
        assert_eq!(payload.len(), 6);
        assert_eq!(payload[0], 0x00);
        assert_eq!(payload[1] & 0x80, 0x00);
        assert_eq!(payload[2], 2);
        assert_eq!(payload[3..5], [0x64, 0x00]);
        assert_eq!(payload[5], payload[..5].iter().fold(0u8, |a, b| a ^ b));
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
    }
}
