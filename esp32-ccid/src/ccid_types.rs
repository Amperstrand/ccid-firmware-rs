//! CCID message types and structures
//!
//! This module contains CCID protocol message definitions, command
//! structures, and response formats per CCID Rev 1.1 specification.

/// PC_to_RDR_IccPowerOn - Apply power to ICC and get ATR
pub const PC_TO_RDR_ICCPOWERON: u8 = 0x62;
/// PC_to_RDR_IccPowerOff - Remove power from ICC
pub const PC_TO_RDR_ICCPOWEROFF: u8 = 0x63;
/// PC_to_RDR_GetSlotStatus - Get slot status
pub const PC_TO_RDR_GETSLOTSTAT: u8 = 0x65;
/// PC_to_RDR_XfrBlock - Transfer data block (APDU)
pub const PC_TO_RDR_XFRBLOCK: u8 = 0x6F;
/// PC_to_RDR_GetParameters - Get protocol parameters
pub const PC_TO_RDR_GETPARAMETERS: u8 = 0x6C;
/// PC_to_RDR_SetParameters - Set protocol parameters
pub const PC_TO_RDR_SETPARAMETERS: u8 = 0x61;
/// PC_to_RDR_ResetParameters - Reset params
pub const PC_TO_RDR_RESETPARAMETERS: u8 = 0x6D;
/// PC_to_RDR_Escape - Vendor-specific command
pub const PC_TO_RDR_ESCAPE: u8 = 0x6B;

/// RDR_to_PC_DataBlock - Response with data (ATR, APDU response)
pub const RDR_TO_PC_DATABLOCK: u8 = 0x80;
/// RDR_to_PC_SlotStatus - Response with slot status
pub const RDR_TO_PC_SLOTSTATUS: u8 = 0x81;
/// RDR_to_PC_Parameters - Response with protocol parameters
pub const RDR_TO_PC_PARAMETERS: u8 = 0x82;
/// RDR_to_PC_Escape - Response to Escape command
pub const RDR_TO_PC_ESCAPE: u8 = 0x83;
/// RDR_to_PC_NotifySlotChange - Interrupt message for slot change
pub const RDR_TO_PC_NOTIFYSLOTCHANGE: u8 = 0x50;

/// ICC present and active (powered on)
pub const ICC_PRESENT_ACTIVE: u8 = 0x00;
/// ICC present but inactive (not powered)
pub const ICC_PRESENT_INACTIVE: u8 = 0x01;
/// No ICC present
pub const ICC_NOT_PRESENT: u8 = 0x02;

/// Command processed successfully
pub const CMD_STATUS_OK: u8 = 0x00;
/// Command failed
pub const CMD_STATUS_FAILED: u8 = 0x40;
/// Command time extension requested
pub const CMD_STATUS_TIME_EXT: u8 = 0x80;

/// Command not supported
pub const CMD_NOT_SUPPORTED: u8 = 0x00;
/// ICC does not respond (mute)
pub const ICC_MUTE: u8 = 0xFE;
/// XFR parity error
pub const XFR_PARITY_ERROR: u8 = 0xFD;
/// XFR overrun
pub const XFR_OVERRUN: u8 = 0xFC;
/// Hardware error
pub const HW_ERROR: u8 = 0xFB;
/// Bad ATR TS byte
pub const BAD_ATR_TS: u8 = 0xF8;
/// Bad ATR TCK byte
pub const BAD_ATR_TCK: u8 = 0xF7;
/// ICC protocol not supported
pub const ICC_PROTOCOL_NOT_SUPPORTED: u8 = 0xF6;
/// ICC class not supported
pub const ICC_CLASS_NOT_SUPPORTED: u8 = 0xF5;
/// Procedure byte conflict
pub const PROCEDURE_BYTE_CONFLICT: u8 = 0xF4;
/// Deactivated protocol
pub const DEACTIVATED_PROTOCOL: u8 = 0xF3;
/// Busy with auto sequence
pub const BUSY_WITH_AUTO_SEQUENCE: u8 = 0xF2;
/// PIN timeout
pub const PIN_TIMEOUT: u8 = 0xF0;
/// PIN cancelled
pub const PIN_CANCELLED: u8 = 0xEF;
/// Slot busy
pub const CMD_SLOT_BUSY: u8 = 0xE0;
pub const ICC_NOT_ACTIVE: u8 = 0x05;

/// Card absent in slot
pub const CARD_ABSENT: u8 = 0x02;
/// Card present in slot
pub const CARD_PRESENT: u8 = 0x03;

/// Maximum CCID message length (header + data)
pub const MAX_CCID_MESSAGE_LENGTH: u32 = 271;
/// Maximum IFSD for T=1 protocol
pub const MAX_IFSD: u32 = 254;
/// Feature flags (Auto PPS, Auto baud, Auto ICC volt, etc.)
pub const FEATURES: u32 = 0x00010230;
/// Default clock frequency in kHz
pub const DEFAULT_CLOCK: u32 = 4000;
/// Maximum data rate in bps
pub const MAX_DATA_RATE: u32 = 344086;
/// Maximum slot index (0 = single slot)
pub const MAX_SLOT_INDEX: u8 = 0;
/// Voltage support: 1.8V, 3V, and 5V
pub const VOLTAGE_SUPPORT: u8 = 0x07;

/// CCID message header (10 bytes per CCID Rev 1.1 specification)
///
/// Wire format (little-endian for dwLength):
/// ```text
/// [0] bMessageType
/// [1..4] dwLength (little-endian u32) - length of payload AFTER header
/// [5] bSlot
/// [6] bSeq
/// [7] bStatus (response) / bBWI (command) / bClockCommand
/// [8] bError (response) / 0x00 (command)
/// [9] bSpecific / bRFU
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct CcidHeader {
    /// Message type (command or response)
    pub message_type: u8,
    /// Length of payload after the header
    pub length: u32,
    /// Slot number (0 for single-slot readers)
    pub slot: u8,
    /// Sequence number
    pub seq: u8,
    /// Status/error/specific bytes
    pub specific: [u8; 3],
}

impl CcidHeader {
    /// Parse a CCID header from bytes
    ///
    /// Returns `None` if the input slice is too short (< 10 bytes)
    pub fn parse(bytes: &[u8]) -> Option<CcidHeader> {
        if bytes.len() < 10 {
            return None;
        }

        Some(CcidHeader {
            message_type: bytes[0],
            length: u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]),
            slot: bytes[5],
            seq: bytes[6],
            specific: [bytes[7], bytes[8], bytes[9]],
        })
    }

    /// Build a CCID header as a 10-byte array
    ///
    /// # Arguments
    /// * `msg_type` - Message type byte
    /// * `length` - Payload length (little-endian in wire format)
    /// * `slot` - Slot number
    /// * `seq` - Sequence number
    /// * `status` - bStatus field (response) or other byte
    /// * `error` - bError field (response) or 0x00 (command)
    /// * `specific` - bSpecific/bRFU byte
    pub fn build(
        msg_type: u8,
        length: u32,
        slot: u8,
        seq: u8,
        status: u8,
        error: u8,
        specific: u8,
    ) -> [u8; 10] {
        let len_bytes = length.to_le_bytes();
        [
            msg_type,
            len_bytes[0],
            len_bytes[1],
            len_bytes[2],
            len_bytes[3],
            slot,
            seq,
            status,
            error,
            specific,
        ]
    }
}

/// Pack slot status byte from ICC status and command status
///
/// # Arguments
/// * `icc_status` - ICC status (0=present/active, 1=present/inactive, 2=not present)
/// * `cmd_status` - Command status (0x00=OK, 0x40=failed, 0x80=time extension)
///
/// # Returns
/// Combined status byte: `(cmd_status & 0xC0) | (icc_status & 0x03)`
pub fn slot_status(icc_status: u8, cmd_status: u8) -> u8 {
    (cmd_status & 0xC0) | (icc_status & 0x03)
}

/// Default T=0 protocol parameters (5 bytes)
///
/// Format per CCID Rev 1.1 Table 6.2-3:
/// - [0] bmFindexDindex: Fi=372, Di=1
/// - [1] bmTCCKST0: convention=direct, checksum=CRC, no special guard time
/// - [2] bGuardTimeT0: extra guard time (0)
/// - [3] bWaitingIntegerT0: waiting integer (0)
/// - [4] bClockStop: 0x00 (clock not stopped)
pub const DEFAULT_T0_PARAMS: [u8; 5] = [0x11, 0x00, 0x00, 0x00, 0x00];

/// Default T=1 protocol parameters (7 bytes)
///
/// Format per CCID Rev 1.1 Table 6.2-3:
/// - [0] bmFindexDindex: Fi=372, Di=1 (no TA1)
/// - [1] bmTCCKST1: convention=direct, EDC=CRC, clock stop=not supported
/// - [2] bGuardTimeT1: extra guard time (0)
/// - [3] bWaitingIntegersT1: BWI=3, CWI=13 (encoded per spec)
/// - [4] bClockStop: 0x00 (clock not stopped)
/// - [5] bIFSC: Information field size for device (32 = 0x20)
/// - [6] bNadValue: Node address (0x00)
pub const DEFAULT_T1_PARAMS: [u8; 7] = [0x11, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ccid_header() {
        let bytes = [0x62, 0x20, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];

        let header = CcidHeader::parse(&bytes).expect("Failed to parse header");

        assert_eq!(header.message_type, 0x62);
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
        let status = slot_status(ICC_PRESENT_ACTIVE, CMD_STATUS_OK);
        assert_eq!(status, 0x00);
    }

    #[test]
    fn test_slot_status_not_present_failed() {
        let status = slot_status(ICC_NOT_PRESENT, CMD_STATUS_FAILED);
        assert_eq!(status, 0x42);
    }

    #[test]
    fn test_slot_status_present_inactive_ok() {
        let status = slot_status(ICC_PRESENT_INACTIVE, CMD_STATUS_OK);
        assert_eq!(status, 0x01);
    }

    #[test]
    fn test_slot_status_present_active_time_ext() {
        let status = slot_status(ICC_PRESENT_ACTIVE, CMD_STATUS_TIME_EXT);
        assert_eq!(status, 0x80);
    }

    #[test]
    fn test_constants_message_types() {
        assert_eq!(PC_TO_RDR_ICCPOWERON, 0x62);
        assert_eq!(PC_TO_RDR_ICCPOWEROFF, 0x63);
        assert_eq!(PC_TO_RDR_GETSLOTSTAT, 0x65);
        assert_eq!(PC_TO_RDR_XFRBLOCK, 0x6F);
        assert_eq!(PC_TO_RDR_GETPARAMETERS, 0x6C);
        assert_eq!(PC_TO_RDR_SETPARAMETERS, 0x61);
        assert_eq!(PC_TO_RDR_RESETPARAMETERS, 0x6D);
        assert_eq!(PC_TO_RDR_ESCAPE, 0x6B);

        assert_eq!(RDR_TO_PC_DATABLOCK, 0x80);
        assert_eq!(RDR_TO_PC_SLOTSTATUS, 0x81);
        assert_eq!(RDR_TO_PC_PARAMETERS, 0x82);
        assert_eq!(RDR_TO_PC_ESCAPE, 0x83);
        assert_eq!(RDR_TO_PC_NOTIFYSLOTCHANGE, 0x50);
    }

    #[test]
    fn test_constants_icc_status() {
        assert_eq!(ICC_PRESENT_ACTIVE, 0x00);
        assert_eq!(ICC_PRESENT_INACTIVE, 0x01);
        assert_eq!(ICC_NOT_PRESENT, 0x02);
    }

    #[test]
    fn test_constants_command_status() {
        assert_eq!(CMD_STATUS_OK, 0x00);
        assert_eq!(CMD_STATUS_FAILED, 0x40);
        assert_eq!(CMD_STATUS_TIME_EXT, 0x80);
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
        assert_eq!(FEATURES, 0x00010230);
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
