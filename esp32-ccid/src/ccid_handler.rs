use crate::ccid_types::{
    build_bstatus, CcidHeader, SlotState, CCID_HEADER_SIZE, CMD_NOT_SUPPORTED,
    COMMAND_STATUS_FAILED, COMMAND_STATUS_NO_ERROR, HW_ERROR, ICC_NOT_ACTIVE, ICC_STATUS_NO_ICC,
    PC_TO_RDR_ESCAPE, PC_TO_RDR_GET_PARAMETERS, PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_POWER_OFF,
    PC_TO_RDR_ICC_POWER_ON, PC_TO_RDR_RESET_PARAMETERS, PC_TO_RDR_SET_PARAMETERS,
    PC_TO_RDR_XFR_BLOCK, RDR_TO_PC_DATABLOCK, RDR_TO_PC_ESCAPE, RDR_TO_PC_PARAMETERS,
};
use crate::nfc::{NfcDriver, PresenceState};
use ccid_core::params::default_params;
use ccid_core::pps::is_pps_request;
use ccid_core::response::{write_message, write_slot_status as shared_write_slot_status};

const FIRMWARE_VERSION: &[u8] = b"GemPC Twin ESP32 1.0\0";

pub struct CcidHandler<D: NfcDriver> {
    nfc: D,
    slot_state: SlotState,
    presence_state: PresenceState,
    tx_buf: [u8; 271],
    sync_notifications: bool,
    current_protocol: u8,
}

impl<D: NfcDriver> CcidHandler<D> {
    pub fn new(nfc: D) -> Self {
        Self {
            nfc,
            slot_state: SlotState::Absent,
            presence_state: PresenceState { present: false },
            tx_buf: [0u8; 271],
            sync_notifications: false,
            current_protocol: 1,
        }
    }

    pub fn process_command(&mut self, ccid_msg: &[u8], response: &mut [u8]) -> usize {
        let Some(header) = CcidHeader::parse(ccid_msg) else {
            return 0;
        };

        let payload_len = header.length as usize;
        if ccid_msg.len() < CCID_HEADER_SIZE + payload_len {
            return shared_write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_FAILED,
                CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }

        let payload = &ccid_msg[CCID_HEADER_SIZE..CCID_HEADER_SIZE + payload_len];

        match header.message_type {
            PC_TO_RDR_ICC_POWER_ON => self.handle_power_on(&header, response),
            PC_TO_RDR_ICC_POWER_OFF => self.handle_power_off(&header, response),
            PC_TO_RDR_GET_SLOT_STATUS => self.handle_get_slot_status(&header, response),
            PC_TO_RDR_XFR_BLOCK => self.handle_xfr_block(&header, payload, response),
            PC_TO_RDR_GET_PARAMETERS => self.write_parameters(&header, response),
            PC_TO_RDR_SET_PARAMETERS => self.handle_set_parameters(&header, response),
            PC_TO_RDR_RESET_PARAMETERS => self.handle_reset_parameters(&header, response),
            PC_TO_RDR_ESCAPE => self.handle_escape(&header, payload, response),
            _ => shared_write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_FAILED,
                CMD_NOT_SUPPORTED,
                0,
                response,
            ),
        }
    }

    pub fn check_card_change(&mut self) -> Option<bool> {
        if self.nfc.session_active() {
            self.presence_state = PresenceState { present: true };
            self.slot_state = SlotState::PresentActive;
            return None;
        }

        let presence = self.nfc.poll_card_presence();
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

    fn handle_power_on(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        if !self.presence_state.present {
            return shared_write_slot_status(
                header.slot,
                header.seq,
                ICC_STATUS_NO_ICC,
                COMMAND_STATUS_FAILED,
                CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }

        match self.nfc.power_on(&mut self.tx_buf) {
            Ok(atr_len) => {
                self.presence_state.present = true;
                self.slot_state = SlotState::PresentActive;
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
            Err(_) => {
                // Don't poll after activation failure — the card may be in an
                // uncertain ISO 14443-3A state (e.g. READY after a partial
                // WUPA). Polling now could send WUPA from READY which keeps
                // the card stuck, breaking the next PowerUp attempt.
                // Instead, assume the card is still physically present and let
                // the next scheduled poll cycle verify.
                self.slot_state = SlotState::PresentInactive;
                shared_write_slot_status(
                    header.slot,
                    header.seq,
                    self.current_icc_status(),
                    COMMAND_STATUS_FAILED,
                    HW_ERROR,
                    0,
                    response,
                )
            }
        }
    }

    fn handle_power_off(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        self.nfc.power_off();

        // After DESELECT the card is in HALT state. Do NOT poll here —
        // WUPA would move it to READY, and the next PowerUp's WUPA would
        // fail (WUPA is only valid from IDLE/HALT, not READY).
        // pcscd does PowerUp→PowerDown→PowerUp as a warm reset sequence,
        // so the next PowerUp must succeed.
        self.slot_state = SlotState::PresentInactive;

        shared_write_slot_status(
            header.slot,
            header.seq,
            self.current_icc_status(),
            COMMAND_STATUS_NO_ERROR,
            0,
            0,
            response,
        )
    }

    fn handle_get_slot_status(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        shared_write_slot_status(
            header.slot,
            header.seq,
            self.current_icc_status(),
            COMMAND_STATUS_NO_ERROR,
            0,
            0,
            response,
        )
    }

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

        if is_pps_request(apdu) {
            log::info!("xfr_block: PPS request, echoing back: {:02X?}", apdu);
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

        match self.nfc.transmit_apdu(apdu, &mut self.tx_buf) {
            Ok(resp_len) => write_message(
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
            ),
            Err(_) => {
                self.slot_state = SlotState::PresentInactive;
                write_message(
                    RDR_TO_PC_DATABLOCK,
                    header.slot,
                    header.seq,
                    build_bstatus(COMMAND_STATUS_FAILED, self.current_icc_status()),
                    HW_ERROR,
                    0,
                    &[],
                    response,
                )
            }
        }
    }

    fn handle_set_parameters(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        self.current_protocol = header.specific[0];
        self.write_parameters(header, response)
    }

    fn handle_reset_parameters(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        self.current_protocol = 1;
        self.write_parameters(header, response)
    }

    fn handle_escape(&mut self, header: &CcidHeader, payload: &[u8], response: &mut [u8]) -> usize {
        if payload.first() == Some(&0x02) {
            return write_message(
                RDR_TO_PC_ESCAPE,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
                0,
                0,
                FIRMWARE_VERSION,
                response,
            );
        }

        if payload == &[0x1F, 0x02] {
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

        shared_write_slot_status(
            header.slot,
            header.seq,
            self.current_icc_status(),
            COMMAND_STATUS_FAILED,
            CMD_NOT_SUPPORTED,
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
    use crate::ccid_types::{
        DEFAULT_T0_PARAMS, DEFAULT_T1_PARAMS, PC_TO_RDR_ESCAPE, PC_TO_RDR_GET_PARAMETERS,
        PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_POWER_OFF, PC_TO_RDR_ICC_POWER_ON,
        PC_TO_RDR_RESET_PARAMETERS, PC_TO_RDR_SET_PARAMETERS, PC_TO_RDR_XFR_BLOCK,
        RDR_TO_PC_SLOTSTATUS,
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
        assert_eq!(handler.slot_state, SlotState::PresentActive);
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
        assert_eq!(handler.slot_state, SlotState::Absent);
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
        assert_eq!(handler.slot_state, SlotState::PresentInactive);
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
        assert_eq!(handler.slot_state, SlotState::PresentInactive);
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
        assert_eq!(handler.slot_state, SlotState::Absent);
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
    fn test_escape_sync_notification_enables_sync_mode() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 10, &[0x01, 0x01, 0x01]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert_eq!(payload, [0x01, 0x01, 0x01]);
        assert!(handler.sync_notifications);
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

        handler.nfc.set_card_present(true);
        assert_eq!(handler.check_card_change(), Some(true));

        handler.slot_state = SlotState::PresentActive;
        handler.nfc.set_card_present(false);
        assert_eq!(handler.check_card_change(), Some(false));
        assert_eq!(handler.slot_state, SlotState::Absent);
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
        assert_eq!(handler.slot_state, SlotState::PresentActive);
        assert!(handler.nfc.session_active());

        let xfr = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 17, &[0x00, 0x84, 0x00, 0x00]);
        let xfr_len = handler.process_command(&xfr, &mut response);
        let (xfr_header, xfr_payload) = parse_response(&response[..xfr_len]);

        assert_eq!(xfr_header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(xfr_payload, APDU_RESPONSE);
        assert_eq!(handler.slot_state, SlotState::PresentActive);
        assert!(handler.nfc.session_active());

        let power_off = build_ccid_cmd(PC_TO_RDR_ICC_POWER_OFF, 0, 18, &[]);
        let power_off_len = handler.process_command(&power_off, &mut response);
        let (power_off_header, power_off_payload) = parse_response(&response[..power_off_len]);

        assert_eq!(power_off_header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert!(power_off_payload.is_empty());
        assert_eq!(handler.slot_state, SlotState::PresentInactive);
        assert!(!handler.nfc.session_active());
    }

    #[test]
    fn test_poll_skips_when_session_active() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);

        assert_eq!(handler.check_card_change(), None);
        assert_eq!(handler.slot_state, SlotState::PresentActive);
        assert!(handler.presence_state.present);
    }

    #[test]
    fn test_apdu_failure_downgrades_to_present_inactive_when_card_remains_present() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);
        handler.nfc.power_off();

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
        assert_eq!(handler.slot_state, SlotState::PresentInactive);
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
        assert_eq!(handler.current_protocol, 0);
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
        assert_eq!(handler.current_protocol, 1);
    }
}
