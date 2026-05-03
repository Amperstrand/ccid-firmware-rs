//! CCID message types and structures
//!
//! This module contains CCID protocol message definitions, command
//! structures, and response formats per CCID Rev 1.1 specification.

pub use ccid_protocol::status::*;
pub use ccid_protocol::types::*;

pub const ICC_NOT_ACTIVE: u8 = 0x05;
pub const MAX_IFSD: usize = 254;

pub const CMD_NOT_SUPPORTED: u8 = CCID_ERR_CMD_NOT_SUPPORTED;
pub const ICC_MUTE: u8 = CCID_ERR_ICC_MUTE;
pub const XFR_PARITY_ERROR: u8 = CCID_ERR_XFR_PARITY_ERROR;
pub const XFR_OVERRUN: u8 = CCID_ERR_XFR_OVERRUN;
pub const HW_ERROR: u8 = CCID_ERR_HW_ERROR;
pub const BAD_ATR_TS: u8 = CCID_ERR_BAD_ATR_TS;
pub const BAD_ATR_TCK: u8 = CCID_ERR_BAD_ATR_TCK;
pub const ICC_PROTOCOL_NOT_SUPPORTED: u8 = CCID_ERR_ICC_PROTOCOL_NOT_SUPPORTED;
pub const ICC_CLASS_NOT_SUPPORTED: u8 = CCID_ERR_ICC_CLASS_NOT_SUPPORTED;
pub const PROCEDURE_BYTE_CONFLICT: u8 = CCID_ERR_PROCEDURE_BYTE_CONFLICT;
pub const DEACTIVATED_PROTOCOL: u8 = CCID_ERR_DEACTIVATED_PROTOCOL;
pub const BUSY_WITH_AUTO_SEQUENCE: u8 = CCID_ERR_BUSY_WITH_AUTO_SEQUENCE;
pub const PIN_TIMEOUT: u8 = CCID_ERR_PIN_TIMEOUT;
pub const PIN_CANCELLED: u8 = CCID_ERR_PIN_CANCELLED;
pub const CMD_SLOT_BUSY: u8 = CCID_ERR_CMD_SLOT_BUSY;

/// Feature flags:
/// 0x00000010 = Automatic ICC clock frequency change
/// 0x00000020 = Automatic baud rate change
/// 0x00000040 = Automatic parameters negotiation (CCID handles PPS internally)
/// 0x00000200 = ?
/// 0x00010000 = TPDU level exchange
/// Contactless readers must set 0x40 to prevent pcscd from sending PPS to the card.
pub const FEATURES: u32 = 0x00010270;
/// Default clock frequency in kHz
pub const DEFAULT_CLOCK: u32 = 4000;
/// Maximum data rate in bps
pub const MAX_DATA_RATE: u32 = 344086;
/// Maximum slot index (0 = single slot)
pub const MAX_SLOT_INDEX: u8 = 0;
/// Voltage support: 1.8V, 3V, and 5V
pub const VOLTAGE_SUPPORT: u8 = 0x07;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ccid_header() {
        let bytes = [0x62, 0x20, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];

        let header = CcidHeader::parse(&bytes).expect("Failed to parse header");

        assert_eq!(header.message_type, PC_TO_RDR_ICC_POWER_ON);
        assert_eq!(header.length, 32);
        assert_eq!(header.slot, 0);
        assert_eq!(header.seq, 1);
        assert_eq!(header.specific, [0x00, 0x00, 0x00]);
    }

    #[test]
    fn test_parse_ccid_header_too_short() {
        let bytes = [0x62, 0x20, 0x00, 0x00];
        assert!(CcidHeader::parse(&bytes).is_none());
    }

    #[test]
    fn test_build_ccid_header() {
        let header = CcidHeader::build(0x80, 0x10, 0, 5, 0x00, 0x00, 0x01);

        assert_eq!(header[0], 0x80);
        assert_eq!(header[1], 0x10);
        assert_eq!(header[2], 0x00);
        assert_eq!(header[3], 0x00);
        assert_eq!(header[4], 0x00);
        assert_eq!(header[5], 0);
        assert_eq!(header[6], 5);
        assert_eq!(header[7], 0x00);
        assert_eq!(header[8], 0x00);
        assert_eq!(header[9], 0x01);
    }

    #[test]
    fn test_header_round_trip() {
        let original = CcidHeader {
            message_type: 0x62,
            length: 1234,
            slot: 0,
            seq: 42,
            specific: [0xAB, 0xCD, 0xEF],
        };

        let bytes = CcidHeader::build(
            original.message_type,
            original.length,
            original.slot,
            original.seq,
            original.specific[0],
            original.specific[1],
            original.specific[2],
        );

        let parsed = CcidHeader::parse(&bytes).expect("Failed to parse round-trip header");

        assert_eq!(parsed, original);
    }

    #[test]
    fn test_slot_status_present_active_ok() {
        let status = build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE);
        assert_eq!(status, 0x00);
    }

    #[test]
    fn test_slot_status_not_present_failed() {
        let status = build_bstatus(COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC);
        assert_eq!(status, 0x42);
    }

    #[test]
    fn test_slot_status_present_inactive_ok() {
        let status = build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_INACTIVE);
        assert_eq!(status, 0x01);
    }

    #[test]
    fn test_slot_status_present_active_time_ext() {
        let status = build_bstatus(COMMAND_STATUS_TIME_EXTENSION, ICC_STATUS_PRESENT_ACTIVE);
        assert_eq!(status, 0x80);
    }

    #[test]
    fn test_constants_message_types() {
        assert_eq!(PC_TO_RDR_ICC_POWER_ON, 0x62);
        assert_eq!(PC_TO_RDR_ICC_POWER_OFF, 0x63);
        assert_eq!(PC_TO_RDR_GET_SLOT_STATUS, 0x65);
        assert_eq!(PC_TO_RDR_XFR_BLOCK, 0x6F);
        assert_eq!(PC_TO_RDR_GET_PARAMETERS, 0x6C);
        assert_eq!(PC_TO_RDR_SET_PARAMETERS, 0x61);
        assert_eq!(PC_TO_RDR_RESET_PARAMETERS, 0x6D);
        assert_eq!(PC_TO_RDR_ESCAPE, 0x6B);

        assert_eq!(RDR_TO_PC_DATABLOCK, 0x80);
        assert_eq!(RDR_TO_PC_SLOTSTATUS, 0x81);
        assert_eq!(RDR_TO_PC_PARAMETERS, 0x82);
        assert_eq!(RDR_TO_PC_ESCAPE, 0x83);
        assert_eq!(RDR_TO_PC_NOTIFY_SLOT_CHANGE, 0x50);
    }

    #[test]
    fn test_constants_icc_status() {
        assert_eq!(ICC_STATUS_PRESENT_ACTIVE, 0x00);
        assert_eq!(ICC_STATUS_PRESENT_INACTIVE, 0x01);
        assert_eq!(ICC_STATUS_NO_ICC, 0x02);
    }

    #[test]
    fn test_constants_command_status() {
        assert_eq!(build_bstatus(COMMAND_STATUS_NO_ERROR, 0), 0x00);
        assert_eq!(build_bstatus(COMMAND_STATUS_FAILED, 0), 0x40);
        assert_eq!(build_bstatus(COMMAND_STATUS_TIME_EXTENSION, 0), 0x80);
    }

    #[test]
    fn test_constants_error_codes() {
        assert_eq!(CMD_NOT_SUPPORTED, 0x00);
        assert_eq!(ICC_MUTE, 0xFE);
        assert_eq!(XFR_OVERRUN, 0xFC);
        assert_eq!(HW_ERROR, 0xFB);
        assert_eq!(BAD_ATR_TS, 0xF8);
        assert_eq!(ICC_CLASS_NOT_SUPPORTED, 0xF5);
    }

    #[test]
    fn test_constants_slot_change() {
        assert_eq!(CARD_ABSENT, 0x02);
        assert_eq!(CARD_PRESENT, 0x03);
    }

    #[test]
    fn test_constants_descriptor() {
        assert_eq!(MAX_CCID_MESSAGE_LENGTH, 271);
        assert_eq!(MAX_IFSD, 254);
        assert_eq!(FEATURES, 0x00010270);
        assert_eq!(DEFAULT_CLOCK, 4000);
        assert_eq!(MAX_DATA_RATE, 344086);
        assert_eq!(MAX_SLOT_INDEX, 0);
        assert_eq!(VOLTAGE_SUPPORT, 0x07);
    }

    #[test]
    fn test_default_params() {
        assert_eq!(DEFAULT_T0_PARAMS.len(), 5);
        assert_eq!(DEFAULT_T0_PARAMS[0], 0x11);
        assert_eq!(DEFAULT_T0_PARAMS[1], 0x00);
        assert_eq!(DEFAULT_T0_PARAMS[2], 0x00);
        assert_eq!(DEFAULT_T0_PARAMS[3], 0x00);
        assert_eq!(DEFAULT_T0_PARAMS[4], 0x00);

        assert_eq!(DEFAULT_T1_PARAMS.len(), 7);
        assert_eq!(DEFAULT_T1_PARAMS[0], 0x11);
        assert_eq!(DEFAULT_T1_PARAMS[1], 0x00);
        assert_eq!(DEFAULT_T1_PARAMS[2], 0x00);
        assert_eq!(DEFAULT_T1_PARAMS[3], 0x03);
        assert_eq!(DEFAULT_T1_PARAMS[4], 0x00);
        assert_eq!(DEFAULT_T1_PARAMS[5], 0x20);
        assert_eq!(DEFAULT_T1_PARAMS[6], 0x00);
    }

    #[test]
    fn test_parse_little_endian_length() {
        let bytes = [0x80, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
        let header = CcidHeader::parse(&bytes).expect("Failed to parse");

        assert_eq!(header.message_type, 0x80);
        assert_eq!(header.length, 256);
    }

    #[test]
    fn test_build_little_endian_length() {
        let header = CcidHeader::build(0x62, 65535, 0, 0, 0x00, 0x00, 0x00);

        assert_eq!(header[1], 0xFF);
        assert_eq!(header[2], 0xFF);
        assert_eq!(header[3], 0x00);
        assert_eq!(header[4], 0x00);
    }
}
