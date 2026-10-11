//! Transport-agnostic CCID Rev 1.1 §6 message handler core.
//!
//! Extracted from the ESP32 `CcidHandler` (the younger, fuzz-hardened of
//! the two parallel handlers — docs/PERF_AND_DRY_STRATEGY.md §3.1) so
//! both firmwares can share one dispatch: slot lifecycle, bStatus
//! assembly, parameter handling, escape diagnostics and the APDU-level
//! pipeline (PPS echo, PC/SC pseudo-APDUs, card relay) live here, behind
//! the card-interface driver traits.
//!
//! What deliberately stays OUT (transport/firmware glue, kept by the
//! thin wrappers):
//! - T=1 endpoint routing (`esp32-ccid/src/t1.rs`, §3.4) — wired via the
//!   [`CcidHandlerCore::take_card_reset`] /
//!   [`CcidHandlerCore::take_parameters_accepted`] events and
//!   [`CcidHandlerCore::apdu_pipeline`]
//! - Secure (PIN verify/modify) — STM32-only today, phase-2 port
//! - Escape 0xD1 snapshot panic — the wrapper acks, the serve loop panics
//!
//! Wire behavior is byte-identical to the pre-extraction ESP32 handler
//! (conformance-battery gate, issue #92 fuzz suite).

use crate::diagnostics::Diagnostics;
use crate::params::default_params;
use crate::pps::is_pps_request;
use crate::response::{write_message, write_slot_status};
use card_interface::{CardBackend, NfcCardExt, PresenceState};
use ccid_protocol::status::build_bstatus;
use ccid_protocol::types::{
    CcidHeader, SlotState, CCID_ERR_CMD_NOT_SUPPORTED, CCID_ERR_HW_ERROR, CCID_HEADER_SIZE,
    COMMAND_STATUS_FAILED, COMMAND_STATUS_NO_ERROR, ICC_STATUS_NO_ICC, PC_TO_RDR_ESCAPE,
    PC_TO_RDR_GET_PARAMETERS, PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_POWER_OFF,
    PC_TO_RDR_ICC_POWER_ON, PC_TO_RDR_RESET_PARAMETERS, PC_TO_RDR_SET_PARAMETERS,
    PC_TO_RDR_XFR_BLOCK, RDR_TO_PC_DATABLOCK, RDR_TO_PC_ESCAPE, RDR_TO_PC_PARAMETERS,
};

/// GemPC Twin serial convention (not a CCID Rev 1.1 Table 6.2-2 code):
/// XfrBlock against a present-but-inactive slot.
pub const ICC_NOT_ACTIVE: u8 = 0x05;

pub struct CcidHandlerCore<D: CardBackend + NfcCardExt> {
    nfc: D,
    slot_state: SlotState,
    presence_state: PresenceState,
    tx_buf: [u8; 271],
    sync_notifications: bool,
    diagnostic_snapshot_requested: bool,
    current_protocol: u8,
    diagnostics: Diagnostics,
    firmware_version: &'static [u8],
    card_reset_event: bool,
    parameters_accepted_event: bool,
}

impl<D: CardBackend + NfcCardExt> CcidHandlerCore<D> {
    /// `firmware_version` is the string returned by Escape 0x02 (reader
    /// identity on the wire — GemPC Twin convention).
    pub fn new(nfc: D, firmware_version: &'static [u8]) -> Self {
        let mut handler = Self {
            nfc,
            slot_state: SlotState::Absent,
            presence_state: PresenceState { present: false },
            tx_buf: [0u8; 271],
            sync_notifications: false,
            diagnostic_snapshot_requested: false,
            // T=1 (contactless ISO-DEP) reader default.
            current_protocol: 1,
            diagnostics: Diagnostics::new(),
            firmware_version,
            card_reset_event: false,
            parameters_accepted_event: false,
        };
        // Sync recovery history at construction (Codex reviews on #46/#42):
        // startup retries may already have reinitialized the driver before
        // the handler existed; without this the first 0xD0 query reports 0.
        handler.diagnostics.reinit_count = handler.nfc.reinit_count();
        handler
    }

    /// Escape 0xD1 arms a diagnostic snapshot: the response is acked, the
    /// serve loop consumes this flag AFTER the ack went out and panics
    /// with a marker reason so the panic handler writes a flash coredump
    /// for post-mortem retrieval (AGENTS.md "Crash dumps & snapshots").
    pub fn take_snapshot_request(&mut self) -> bool {
        core::mem::replace(&mut self.diagnostic_snapshot_requested, false)
    }

    /// Consumed by transport glue after every delegated command: set on
    /// each successful IccPowerOn. A card reset invalidates the
    /// negotiated protocol (CCID §6.1.2: parameters do not survive ATR
    /// reset) — protocol-dependent routes (e.g. the ESP32 T=1 endpoint)
    /// must disarm; SetParameters re-arms via
    /// [`Self::take_parameters_accepted`].
    pub fn take_card_reset(&mut self) -> bool {
        core::mem::replace(&mut self.card_reset_event, false)
    }

    /// Consumed by transport glue after every delegated command: set when
    /// SetParameters/ResetParameters was ACCEPTED (invalid bProtocolNum
    /// rejections never set it).
    pub fn take_parameters_accepted(&mut self) -> bool {
        core::mem::replace(&mut self.parameters_accepted_event, false)
    }

    /// Refresh time-varying diagnostic fields. Called from the firmware's
    /// poll loop; the handler stays host-testable by taking the tick count
    /// as a parameter instead of reading a platform timer itself.
    pub fn refresh_diagnostics(&mut self, uptime_ticks: u32) {
        self.diagnostics.uptime_ticks = uptime_ticks;
        self.diagnostics.reinit_count = self.nfc.reinit_count();
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    pub fn driver_mut(&mut self) -> &mut D {
        &mut self.nfc
    }

    pub fn diagnostics_mut(&mut self) -> &mut Diagnostics {
        &mut self.diagnostics
    }

    pub fn record_nak(&mut self) {
        self.diagnostics.nak_count = self.diagnostics.nak_count.saturating_add(1);
    }

    /// Counts one protocol error (truncation guard, failed responses) —
    /// the same accounting `process_command` applies internally, exposed
    /// for wrapper-side dispatch paths (T=1 route).
    pub fn record_protocol_error(&mut self) {
        self.diagnostics.error_count = self.diagnostics.error_count.saturating_add(1);
    }

    pub fn slot_state(&self) -> SlotState {
        self.slot_state
    }

    pub fn current_protocol(&self) -> u8 {
        self.current_protocol
    }

    /// Mirrors the protocol-state effect of an accepted SetParameters
    /// (without emitting a response) — host tooling / tests.
    pub fn set_current_protocol(&mut self, protocol: u8) {
        self.current_protocol = protocol;
    }

    pub fn presence_state(&self) -> PresenceState {
        self.presence_state
    }

    pub fn sync_notifications(&self) -> bool {
        self.sync_notifications
    }

    pub fn process_command(&mut self, ccid_msg: &[u8], response: &mut [u8]) -> usize {
        let Some(header) = CcidHeader::parse(ccid_msg) else {
            return 0;
        };

        let payload_len = header.length as usize;
        if ccid_msg.len() < CCID_HEADER_SIZE + payload_len {
            // Input shorter than its declared dwLength. WIRE frames can
            // never arrive here truncated: the serial server's frame
            // parser only emits complete frames and its inter-byte
            // stall guard abandons partial ones before dispatch (issue
            // #114, Codex on #93). This path guards DIRECT
            // process_command input (host tests, the handler fuzz
            // suite) and counts a protocol error (Codex reviews on
            // #40/#38); the early return bypasses the post-dispatch
            // status accounting below.
            self.record_protocol_error();
            return write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_FAILED,
                CCID_ERR_CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }

        let payload = &ccid_msg[CCID_HEADER_SIZE..CCID_HEADER_SIZE + payload_len];

        let len = match header.message_type {
            PC_TO_RDR_ICC_POWER_ON => self.handle_power_on(&header, response),
            PC_TO_RDR_ICC_POWER_OFF => self.handle_power_off(&header, response),
            PC_TO_RDR_GET_SLOT_STATUS => self.handle_get_slot_status(&header, response),
            PC_TO_RDR_XFR_BLOCK => self.handle_xfr_block(&header, payload, response),
            PC_TO_RDR_GET_PARAMETERS => self.write_parameters(&header, response),
            PC_TO_RDR_SET_PARAMETERS => self.handle_set_parameters(&header, response),
            PC_TO_RDR_RESET_PARAMETERS => self.handle_reset_parameters(&header, response),
            PC_TO_RDR_ESCAPE => self.handle_escape(&header, payload, response),
            _ => write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_FAILED,
                CCID_ERR_CMD_NOT_SUPPORTED,
                0,
                response,
            ),
        };

        if len > 7 && response[7] & 0x40 != 0 {
            self.diagnostics.error_count = self.diagnostics.error_count.saturating_add(1);
        }

        len
    }

    pub fn check_card_change(&mut self) -> Option<bool> {
        if self.nfc.session_active() {
            self.presence_state = PresenceState { present: true };
            self.slot_state = SlotState::PresentActive;
            self.diagnostics.card_present = true;
            return None;
        }

        let presence = self.nfc.poll_card_presence();
        self.diagnostics.card_present = presence.present;
        self.diagnostics.reinit_count = self.nfc.reinit_count();
        if presence.present != self.presence_state.present {
            self.presence_state = presence;
            self.slot_state = if presence.present {
                SlotState::PresentInactive
            } else {
                SlotState::Absent
            };
            Some(presence.present)
        } else {
            None
        }
    }

    // CCID_SPEC: /* Section 6.1.1 */ struct ccid_pc_to_rdr_icc_power_on {
    // struct ccid_header hdr; uint8_t bPowerSelect; uint8_t abRFU[2]; }
    // __attribute__ ((packed)); /* Response: RDR_to_PC_DataBlock */
    fn handle_power_on(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        if !self.presence_state.present {
            return write_slot_status(
                header.slot,
                header.seq,
                ICC_STATUS_NO_ICC,
                COMMAND_STATUS_FAILED,
                CCID_ERR_CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }

        match self.nfc.power_on(&mut self.tx_buf) {
            Ok(atr_len) => {
                self.presence_state.present = true;
                self.slot_state = SlotState::PresentActive;
                self.card_reset_event = true;
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
                    &self.tx_buf[..atr_len],
                    response,
                )
            }
            Err(e) => {
                // Bring-up diagnostics (bench 2026-10-08): the activation
                // failure reason is otherwise invisible.
                log::warn!("power_on failed: {:?}", e);
                // Don't poll after activation failure — the card may be in an
                // uncertain ISO 14443-3A state (e.g. READY after a partial
                // WUPA). Polling now could send WUPA from READY which keeps
                // the card stuck, breaking the next PowerUp attempt.
                // Instead, assume the card is still physically present and let
                // the next scheduled poll cycle verify.
                self.slot_state = SlotState::PresentInactive;
                write_slot_status(
                    header.slot,
                    header.seq,
                    self.current_icc_status(),
                    COMMAND_STATUS_FAILED,
                    CCID_ERR_HW_ERROR,
                    0,
                    response,
                )
            }
        }
    }

    // CCID_SPEC: /* Section 6.1.2 */ struct ccid_pc_to_rdr_icc_power_off {
    // struct ccid_header hdr; uint8_t abRFU[3]; } __attribute__ ((packed));
    // /* Response: RDR_to_PC_SlotStatus */
    fn handle_power_off(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        self.nfc.power_off();

        // After DESELECT the card is in HALT state. Do NOT poll here —
        // WUPA would move it to READY, and the next PowerUp's WUPA would
        // fail (WUPA is only valid from IDLE/HALT, not READY).
        // pcscd does PowerUp→PowerDown→PowerUp as a warm reset sequence,
        // so the next PowerUp must succeed.
        self.slot_state = SlotState::PresentInactive;

        write_slot_status(
            header.slot,
            header.seq,
            self.current_icc_status(),
            COMMAND_STATUS_NO_ERROR,
            0,
            0,
            response,
        )
    }

    // CCID_SPEC: /* Section 6.1.3 */ struct ccid_pc_to_rdr_get_slot_status {
    // struct ccid_header hdr; uint8_t abRFU[3]; } __attribute__ ((packed));
    // /* Response: RDR_to_PC_SlotStatus */
    fn handle_get_slot_status(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        write_slot_status(
            header.slot,
            header.seq,
            self.current_icc_status(),
            COMMAND_STATUS_NO_ERROR,
            0,
            0,
            response,
        )
    }

    // GEMPC: dwMaxCCIDMessageLength: 271 bytes
    fn handle_xfr_block(&mut self, header: &CcidHeader, apdu: &[u8], response: &mut [u8]) -> usize {
        if self.slot_state != SlotState::PresentActive {
            return write_message(
                RDR_TO_PC_DATABLOCK,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_FAILED, self.current_icc_status()),
                ICC_NOT_ACTIVE,
                0,
                &[],
                response,
            );
        }

        self.apdu_pipeline(apdu, response)
    }

    fn answer_get_uid(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        let Some(uid) = self.nfc.card_uid() else {
            return self.write_sw(header, &[0x63, 0x00], response);
        };
        let uid_len = uid.len();
        self.tx_buf[..uid_len].copy_from_slice(uid);
        self.tx_buf[uid_len] = 0x90;
        self.tx_buf[uid_len + 1] = 0x00;
        // Local reader-generated answer — no card I/O, so no rx counting
        // (Codex review on #81: to/from-card semantics).
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
            &self.tx_buf[..uid_len + 2],
            response,
        )
    }

    fn write_sw(&mut self, header: &CcidHeader, sw: &[u8; 2], response: &mut [u8]) -> usize {
        // Local reader-generated status word — no card I/O, no rx counting.
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
            sw,
            response,
        )
    }

    fn handle_set_parameters(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        // CCID Rev 1.1 §6.1.7: bProtocolNum 0 = T=0, 1 = T=1; any other
        // value is invalid. Reject with CMD_NOT_SUPPORTED (SlotStatus
        // failed) instead of storing/echoing a bogus protocol that a
        // protocol-dependent route (T=1 endpoint) would then silently
        // fall back from (issue #114, Codex on #93).
        if header.specific[0] > 1 {
            return write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_FAILED,
                CCID_ERR_CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }
        self.current_protocol = header.specific[0];
        self.parameters_accepted_event = true;
        self.write_parameters(header, response)
    }

    fn handle_reset_parameters(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        // Reset restores T=1 (contactless reader default).
        self.current_protocol = 1;
        self.parameters_accepted_event = true;
        self.write_parameters(header, response)
    }

    /// APDU-level pipeline shared by the direct XfrBlock path and
    /// transport-glue relay paths (the ESP32 T=1 route re-enters it):
    /// pseudo-APDUs (0xFF), PPS echo, card relay. Writes CCID DataBlock
    /// bytes into `out` (10-byte header + payload) and returns the total
    /// length; T=1 callers extract the payload for block framing.
    pub fn apdu_pipeline(&mut self, apdu: &[u8], out: &mut [u8]) -> usize {
        let header = &CcidHeader {
            message_type: 0,
            length: 0,
            slot: 0,
            seq: 0,
            specific: [0; 3],
        };
        let response = out;
        if is_pps_request(apdu) {
            log::info!("xfr_block: PPS request, echoing back: {:02X?}", apdu);
            // PPS is handled locally (echoed), not forwarded to the card —
            // per the Diagnostics field docs (APDUs to/from the card) it is
            // deliberately NOT counted in apdu_tx/rx.
            return write_message(
                RDR_TO_PC_DATABLOCK,
                header.slot,
                header.seq,
                build_bstatus(
                    COMMAND_STATUS_NO_ERROR,
                    SlotState::PresentActive.icc_status(),
                ),
                0,
                0,
                apdu,
                response,
            );
        }

        // PC/SC pseudo-APDUs (CLA=0xFF) are answered by the reader itself,
        // never forwarded to the card. CLA=0x00 (ISO 7816) and other CLA
        // values still go to the card — the ACR1252 only intercepts 0xFF.
        // A complete 4-byte header is required; shorter fragments are not
        // routable and keep flowing to the card (issue #49, issue #50).
        if apdu.first() == Some(&0xFF) && apdu.len() >= 4 {
            return match (apdu[1], apdu[2], apdu[3]) {
                (0xCA, 0x00, 0x00) => self.answer_get_uid(header, response),
                (0xCA, _, _) => self.write_sw(header, &[0x6A, 0x86], response),
                _ => self.write_sw(header, &[0x63, 0x00], response),
            };
        }

        // Counted at the moment of actual card transfer (Codex reviews on
        // #46/#43/#38): rejected/inactive requests and locally echoed PPS
        // frames are not APDUs sent to the card.
        self.diagnostics.apdu_tx_count = self.diagnostics.apdu_tx_count.saturating_add(1);
        match self.nfc.transmit_apdu(apdu, &mut self.tx_buf) {
            Ok(resp_len) => {
                self.diagnostics.apdu_rx_count = self.diagnostics.apdu_rx_count.saturating_add(1);
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
                    &self.tx_buf[..resp_len],
                    response,
                )
            }
            Err(_) => {
                self.slot_state = SlotState::PresentInactive;
                write_message(
                    RDR_TO_PC_DATABLOCK,
                    header.slot,
                    header.seq,
                    build_bstatus(COMMAND_STATUS_FAILED, self.current_icc_status()),
                    CCID_ERR_HW_ERROR,
                    0,
                    &[],
                    response,
                )
            }
        }
    }

    // CCID_SPEC: /* Section 6.1.8 */ struct ccid_pc_to_rdr_escape {
    // struct ccid_header hdr; uint8_t abRFU[3]; uint8_t abData[0]; }
    // __attribute__ ((packed)); /* Response: RDR_to_PC_Escape */
    fn handle_escape(&mut self, header: &CcidHeader, payload: &[u8], response: &mut [u8]) -> usize {
        if payload.first() == Some(&0xD0) {
            let mut diag_buf = [0u8; Diagnostics::SERIALIZED_SIZE];
            self.diagnostics.to_bytes(&mut diag_buf);
            return write_message(
                RDR_TO_PC_ESCAPE,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
                0,
                0,
                &diag_buf,
                response,
            );
        }

        // Dump-and-retrieve debugging: ack, then the serve loop panics with
        // a marker reason after this response goes out — the panic handler
        // writes the flash coredump (retrieve + decode: AGENTS.md).
        if payload.first() == Some(&0xD1) {
            self.diagnostic_snapshot_requested = true;
            return write_message(
                RDR_TO_PC_ESCAPE,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
                0,
                0,
                &[0xD1],
                response,
            );
        }

        // CCID_SERIAL: tx_buffer[0] = 0x02; // get reader firmware
        if payload.first() == Some(&0x02) {
            return write_message(
                RDR_TO_PC_ESCAPE,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
                0,
                0,
                self.firmware_version,
                response,
            );
        }

        if payload == [0x1F, 0x02] {
            return write_message(
                RDR_TO_PC_ESCAPE,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
                0,
                0,
                &[],
                response,
            );
        }

        // CCID_SERIAL: perform a command to configure GemPC Twin reader card movement
        // * notification to synchronous mode: the card movement is notified _after_
        // * the host command and _before_ the reader answer
        // CCID_SERIAL: unsigned char tx_buffer[] = { 0x01, 0x01, 0x01};
        if payload.starts_with(&[0x01, 0x01, 0x01]) {
            self.sync_notifications = true;
            return write_message(
                RDR_TO_PC_ESCAPE,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
                0,
                0,
                &[0x01, 0x01, 0x01],
                response,
            );
        }

        write_slot_status(
            header.slot,
            header.seq,
            self.current_icc_status(),
            COMMAND_STATUS_FAILED,
            CCID_ERR_CMD_NOT_SUPPORTED,
            0,
            response,
        )
    }

    fn write_parameters(&self, header: &CcidHeader, response: &mut [u8]) -> usize {
        let (payload, protocol) = self.parameter_payload();
        write_message(
            RDR_TO_PC_PARAMETERS,
            header.slot,
            header.seq,
            build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
            0,
            protocol,
            payload,
            response,
        )
    }

    fn current_icc_status(&self) -> u8 {
        self.slot_state.icc_status()
    }

    fn parameter_payload(&self) -> (&[u8], u8) {
        (default_params(self.current_protocol), self.current_protocol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccid_protocol::types::{DEFAULT_T0_PARAMS, DEFAULT_T1_PARAMS, RDR_TO_PC_SLOTSTATUS};
    use std::vec::Vec;

    const ATR: [u8; 5] = [0x3B, 0x80, 0x80, 0x01, 0x01];
    const APDU_RESPONSE: [u8; 2] = [0x90, 0x00];
    const FIRMWARE_VERSION: &[u8] = b"core-test-reader 0.1\0";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestError {
        NoCard,
        NotInitialized,
        BufferOverflow,
    }

    /// Mirrors the ESP32 `MockNfcDriver` semantics against the shared
    /// card-interface traits.
    #[derive(Debug, Clone)]
    struct TestDriver {
        card_present: bool,
        atr: Vec<u8>,
        apdu_response: Vec<u8>,
        session_active: bool,
        uid: Vec<u8>,
        fail_power_on: bool,
    }

    impl TestDriver {
        fn new(card_present: bool, atr: &[u8], apdu_response: &[u8]) -> Self {
            Self {
                card_present,
                atr: atr.to_vec(),
                apdu_response: apdu_response.to_vec(),
                session_active: false,
                uid: Vec::new(),
                fail_power_on: false,
            }
        }

        fn set_uid(&mut self, uid: &[u8]) {
            self.uid = uid.to_vec();
        }

        fn set_card_present(&mut self, present: bool) {
            self.card_present = present;
            if !present {
                self.session_active = false;
            }
        }
    }

    impl CardBackend for TestDriver {
        type Error = TestError;

        fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
            if self.fail_power_on {
                return Err(TestError::NoCard);
            }
            if !self.card_present {
                self.session_active = false;
                return Err(TestError::NoCard);
            }
            if atr_buf.len() < self.atr.len() {
                return Err(TestError::BufferOverflow);
            }
            atr_buf[..self.atr.len()].copy_from_slice(&self.atr);
            self.session_active = true;
            Ok(self.atr.len())
        }

        fn power_off(&mut self) {
            self.session_active = false;
        }

        fn is_card_present(&mut self) -> bool {
            self.card_present
        }

        fn transmit_apdu(
            &mut self,
            _command: &[u8],
            response: &mut [u8],
        ) -> Result<usize, Self::Error> {
            if !self.card_present {
                self.session_active = false;
                return Err(TestError::NoCard);
            }
            if !self.session_active {
                return Err(TestError::NotInitialized);
            }
            if response.len() < self.apdu_response.len() {
                return Err(TestError::BufferOverflow);
            }
            response[..self.apdu_response.len()].copy_from_slice(&self.apdu_response);
            Ok(self.apdu_response.len())
        }
    }

    impl NfcCardExt for TestDriver {
        fn init(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_card_presence(&mut self) -> PresenceState {
            if !self.card_present {
                self.session_active = false;
            }
            PresenceState {
                present: self.card_present,
            }
        }

        fn session_active(&self) -> bool {
            self.session_active
        }

        fn card_uid(&self) -> Option<&[u8]> {
            (!self.uid.is_empty()).then_some(&self.uid)
        }
    }

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

    fn new_handler(card_present: bool) -> CcidHandlerCore<TestDriver> {
        CcidHandlerCore::new(
            TestDriver::new(card_present, &ATR, &APDU_RESPONSE),
            FIRMWARE_VERSION,
        )
    }

    #[test]
    fn test_power_on_with_card_returns_atr() {
        let mut handler = new_handler(true);
        handler.check_card_change();
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
        assert_eq!(header.specific[1], CCID_ERR_CMD_NOT_SUPPORTED);
        assert_eq!(handler.slot_state(), SlotState::Absent);
    }

    #[test]
    fn test_power_on_success_signals_card_reset_exactly_once() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let mut response = [0u8; 271];

        assert!(!handler.take_card_reset());
        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&cmd, &mut response);
        assert!(handler.take_card_reset());
        assert!(!handler.take_card_reset());
    }

    #[test]
    fn test_power_on_failure_does_not_signal_card_reset() {
        // Present-but-activation-fails (the real PN7160 bench case): the
        // event must fire only on SUCCESSFUL resets, since the wrapper
        // uses it to disarm protocol routes.
        let mut handler = new_handler(true);
        handler.check_card_change();
        handler.driver_mut().fail_power_on = true;
        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        let mut response = [0u8; 271];

        handler.process_command(&cmd, &mut response);
        assert!(!handler.take_card_reset());
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
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
    fn test_escape_firmware_returns_injected_version_string() {
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
        assert!(handler.take_snapshot_request());
        assert!(!handler.take_snapshot_request());
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
        assert_eq!(header.specific[1], CCID_ERR_CMD_NOT_SUPPORTED);
    }

    #[test]
    fn test_check_card_change_detects_insertion_and_removal() {
        let mut handler = new_handler(false);

        assert_eq!(handler.check_card_change(), None);

        handler.driver_mut().set_card_present(true);
        assert_eq!(handler.check_card_change(), Some(true));

        // full activation, then removal
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 30, &[]);
        let mut response = [0u8; 271];
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
        assert!(handler.take_parameters_accepted());
    }

    #[test]
    fn test_set_parameters_rejects_invalid_protocol_num() {
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
        assert_eq!(header.specific[1], CCID_ERR_CMD_NOT_SUPPORTED);
        assert_eq!(handler.current_protocol(), 1);
        assert!(!handler.take_parameters_accepted());
    }

    #[test]
    fn test_reset_parameters_restores_t1_defaults() {
        let mut handler = new_handler(true);
        let set_cmd = build_set_parameters_cmd(0, 14, 0, &DEFAULT_T0_PARAMS);
        let mut response = [0u8; 271];
        handler.process_command(&set_cmd, &mut response);
        assert!(handler.take_parameters_accepted());

        let reset_cmd = build_ccid_cmd(PC_TO_RDR_RESET_PARAMETERS, 0, 15, &[]);
        let len = handler.process_command(&reset_cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_PARAMETERS);
        assert_eq!(header.specific[2], 1);
        assert_eq!(payload, DEFAULT_T1_PARAMS);
        assert_eq!(handler.current_protocol(), 1);
        assert!(handler.take_parameters_accepted());
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

    fn new_handler_with_uid(card_present: bool) -> CcidHandlerCore<TestDriver> {
        let mut nfc = TestDriver::new(card_present, &ATR, &APDU_RESPONSE);
        nfc.set_uid(&CARD_UID);
        CcidHandlerCore::new(nfc, FIRMWARE_VERSION)
    }

    fn power_on(handler: &mut CcidHandlerCore<TestDriver>) {
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
    fn test_truncated_input_counts_single_error_and_answers_failed_status() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        assert_eq!(handler.diagnostics().error_count, 0);

        let mut cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 40, &[0x00, 0xA4]);
        cmd[1] = 0xFF; // claim 255 bytes, deliver 2
        let mut response = [0u8; 512];
        let len = handler.process_command(&cmd, &mut response);
        let (header, _) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_FAILED,
                SlotState::PresentInactive.icc_status()
            )
        );
        // exactly one count: the truncation guard (early return bypasses
        // the post-dispatch accounting)
        assert_eq!(handler.diagnostics().error_count, 1);
    }
}
