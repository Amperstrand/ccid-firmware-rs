//! CCID protocol constants, message types, and data structures per CCID Rev 1.1.

// PC_to_RDR message types
pub const PC_TO_RDR_ICC_POWER_ON: u8 = 0x62;
pub const PC_TO_RDR_ICC_POWER_OFF: u8 = 0x63;
pub const PC_TO_RDR_GET_SLOT_STATUS: u8 = 0x65;
pub const PC_TO_RDR_XFR_BLOCK: u8 = 0x6F;
pub const PC_TO_RDR_GET_PARAMETERS: u8 = 0x6C;
pub const PC_TO_RDR_SET_PARAMETERS: u8 = 0x61;
pub const PC_TO_RDR_SECURE: u8 = 0x69;
pub const PC_TO_RDR_T0_APDU: u8 = 0x6A;
pub const PC_TO_RDR_ESCAPE: u8 = 0x6B;
pub const PC_TO_RDR_RESET_PARAMETERS: u8 = 0x6D;
pub const PC_TO_RDR_ICC_CLOCK: u8 = 0x6E;
pub const PC_TO_RDR_MECHANICAL: u8 = 0x71;
pub const PC_TO_RDR_ABORT: u8 = 0x72;
pub const PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ: u8 = 0x73;

// RDR_to_PC message types
pub const RDR_TO_PC_DATABLOCK: u8 = 0x80;
pub const RDR_TO_PC_SLOTSTATUS: u8 = 0x81;
pub const RDR_TO_PC_PARAMETERS: u8 = 0x82;
pub const RDR_TO_PC_ESCAPE: u8 = 0x83;
pub const RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ: u8 = 0x84;
pub const RDR_TO_PC_NOTIFY_SLOT_CHANGE: u8 = 0x50;

// USB class-specific requests
pub const REQUEST_ABORT: u8 = 0x01;
pub const REQUEST_GET_CLOCK_FREQUENCIES: u8 = 0x02;
pub const REQUEST_GET_DATA_RATES: u8 = 0x03;

// CCID descriptor constants
pub const CLASS_CCID: u8 = 0x0B;
pub const SUBCLASS_NONE: u8 = 0x00;
pub const PROTOCOL_BULK: u8 = 0x00;
pub const DESCRIPTOR_TYPE_CCID: u8 = 0x21;
pub const PACKET_SIZE: usize = 64;
pub const CCID_HEADER_SIZE: usize = 10;
pub const MAX_CCID_MESSAGE_LENGTH: usize = 271;

// ICC status codes (bmICCStatus, 2 bits)
pub const ICC_STATUS_PRESENT_ACTIVE: u8 = 0x00;
pub const ICC_STATUS_PRESENT_INACTIVE: u8 = 0x01;
pub const ICC_STATUS_NO_ICC: u8 = 0x02;

// Command status codes (bmCommandStatus, 2 bits)
pub const COMMAND_STATUS_NO_ERROR: u8 = 0x00;
pub const COMMAND_STATUS_FAILED: u8 = 0x01;
pub const COMMAND_STATUS_TIME_EXTENSION: u8 = 0x02;

// CCID error codes (bError field)
pub const CCID_ERR_CMD_NOT_SUPPORTED: u8 = 0x00;
pub const CCID_ERR_CMD_SLOT_BUSY: u8 = 0xE0;
pub const CCID_ERR_PIN_CANCELLED: u8 = 0xEF;
pub const CCID_ERR_PIN_TIMEOUT: u8 = 0xF0;
pub const CCID_ERR_BUSY_WITH_AUTO_SEQUENCE: u8 = 0xF2;
pub const CCID_ERR_DEACTIVATED_PROTOCOL: u8 = 0xF3;
pub const CCID_ERR_PROCEDURE_BYTE_CONFLICT: u8 = 0xF4;
pub const CCID_ERR_ICC_CLASS_NOT_SUPPORTED: u8 = 0xF5;
pub const CCID_ERR_ICC_PROTOCOL_NOT_SUPPORTED: u8 = 0xF6;
pub const CCID_ERR_BAD_ATR_TCK: u8 = 0xF7;
pub const CCID_ERR_BAD_ATR_TS: u8 = 0xF8;
pub const CCID_ERR_HW_ERROR: u8 = 0xFB;
pub const CCID_ERR_XFR_OVERRUN: u8 = 0xFC;
pub const CCID_ERR_XFR_PARITY_ERROR: u8 = 0xFD;
pub const CCID_ERR_ICC_MUTE: u8 = 0xFE;
pub const CCID_ERR_CMD_ABORTED: u8 = 0xFF;

// NotifySlotChange slot change byte
pub const CARD_ABSENT: u8 = 0x02;
pub const CARD_PRESENT: u8 = 0x03;

// Default T=0 protocol parameters (5 bytes) per CCID Rev 1.1 Table 6.2-3
pub const DEFAULT_T0_PARAMS: [u8; 5] = [0x11, 0x00, 0x00, 0x00, 0x00];

// Default T=1 protocol parameters (7 bytes) per CCID Rev 1.1 Table 6.2-3
pub const DEFAULT_T1_PARAMS: [u8; 7] = [0x11, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Absent,
    PresentInactive,
    PresentActive,
}

impl SlotState {
    pub fn icc_status(&self) -> u8 {
        match self {
            SlotState::PresentActive => ICC_STATUS_PRESENT_ACTIVE,
            SlotState::PresentInactive => ICC_STATUS_PRESENT_INACTIVE,
            SlotState::Absent => ICC_STATUS_NO_ICC,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CcidHeader {
    pub message_type: u8,
    pub length: u32,
    pub slot: u8,
    pub seq: u8,
    pub specific: [u8; 3],
}

impl CcidHeader {
    pub fn parse(bytes: &[u8]) -> Option<CcidHeader> {
        if bytes.len() < CCID_HEADER_SIZE {
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

    pub fn build(
        msg_type: u8,
        length: u32,
        slot: u8,
        seq: u8,
        status: u8,
        error: u8,
        specific: u8,
    ) -> [u8; CCID_HEADER_SIZE] {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ccid_header() {
        let bytes = [0x62, 0x20, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00];
        let header = CcidHeader::parse(&bytes).unwrap();
        assert_eq!(header.message_type, PC_TO_RDR_ICC_POWER_ON);
        assert_eq!(header.length, 32);
        assert_eq!(header.slot, 0);
        assert_eq!(header.seq, 1);
    }

    #[test]
    fn test_parse_too_short() {
        assert!(CcidHeader::parse(&[0x62, 0x20, 0x00, 0x00]).is_none());
    }

    #[test]
    fn test_build_round_trip() {
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
        let parsed = CcidHeader::parse(&bytes).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn test_slot_state_icc_status() {
        assert_eq!(SlotState::Absent.icc_status(), ICC_STATUS_NO_ICC);
        assert_eq!(
            SlotState::PresentInactive.icc_status(),
            ICC_STATUS_PRESENT_INACTIVE
        );
        assert_eq!(
            SlotState::PresentActive.icc_status(),
            ICC_STATUS_PRESENT_ACTIVE
        );
    }

    #[test]
    fn test_default_params() {
        assert_eq!(DEFAULT_T0_PARAMS, [0x11, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(
            DEFAULT_T1_PARAMS,
            [0x11, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00]
        );
    }

    #[test]
    fn test_all_command_constants() {
        assert_eq!(PC_TO_RDR_ICC_POWER_ON, 0x62);
        assert_eq!(PC_TO_RDR_ICC_POWER_OFF, 0x63);
        assert_eq!(PC_TO_RDR_GET_SLOT_STATUS, 0x65);
        assert_eq!(PC_TO_RDR_XFR_BLOCK, 0x6F);
        assert_eq!(PC_TO_RDR_SECURE, 0x69);
        assert_eq!(PC_TO_RDR_ESCAPE, 0x6B);
        assert_eq!(RDR_TO_PC_DATABLOCK, 0x80);
        assert_eq!(RDR_TO_PC_SLOTSTATUS, 0x81);
        assert_eq!(RDR_TO_PC_NOTIFY_SLOT_CHANGE, 0x50);
    }

    /// Simple xorshift32 PRNG — deterministic, zero dependencies.
    struct Xorshift32(u32);
    impl Xorshift32 {
        fn next_u32(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.0 = x;
            x
        }
        fn next_u8(&mut self) -> u8 {
            self.next_u32() as u8
        }
        fn fill_bytes(&mut self, buf: &mut [u8]) {
            for b in buf.iter_mut() {
                *b = self.next_u8();
            }
        }
    }

    #[test]
    fn test_parse_random_bytes_no_panic() {
        let mut rng = Xorshift32(0xDEAD_BEEF);
        for _ in 0..1000 {
            let mut buf = [0u8; 10];
            rng.fill_bytes(&mut buf);
            let _ = CcidHeader::parse(&buf); // must not panic
        }
    }

    #[test]
    fn test_parse_short_inputs() {
        for len in 0..10 {
            let buf = [0u8; 10];
            assert!(
                CcidHeader::parse(&buf[..len]).is_none(),
                "expected None for input of length {}",
                len
            );
        }
    }

    #[test]
    fn test_parse_exactly_10_bytes_always_succeeds() {
        let mut rng = Xorshift32(0xCAFE_F00D);
        for _ in 0..100 {
            let mut buf = [0u8; 10];
            rng.fill_bytes(&mut buf);
            assert!(
                CcidHeader::parse(&buf).is_some(),
                "expected Some for 10-byte input"
            );
        }
    }

    #[test]
    fn test_build_then_parse_roundtrip() {
        let mut rng = Xorshift32(0x1234_5678);
        for _ in 0..100 {
            let msg_type = rng.next_u8();
            let length = rng.next_u32();
            let slot = rng.next_u8();
            let seq = rng.next_u8();
            let status = rng.next_u8();
            let error = rng.next_u8();
            let specific = rng.next_u8();

            let built = CcidHeader::build(msg_type, length, slot, seq, status, error, specific);
            let parsed = CcidHeader::parse(&built).unwrap();

            assert_eq!(parsed.message_type, msg_type);
            assert_eq!(parsed.length, length);
            assert_eq!(parsed.slot, slot);
            assert_eq!(parsed.seq, seq);
            assert_eq!(parsed.specific, [status, error, specific]);
        }
    }

    #[test]
    fn test_header_length_field_max_value() {
        let built = CcidHeader::build(0x62, u32::MAX, 0, 0, 0, 0, 0);
        let parsed = CcidHeader::parse(&built).unwrap();
        assert_eq!(parsed.length, u32::MAX);
    }

    #[test]
    fn test_header_length_field_zero() {
        let built = CcidHeader::build(0x62, 0, 0, 0, 0, 0, 0);
        let parsed = CcidHeader::parse(&built).unwrap();
        assert_eq!(parsed.length, 0);
    }

    #[test]
    fn test_header_length_max_apdu_response() {
        let built = CcidHeader::build(0x80, 261, 0, 0, 0, 0, 0);
        let parsed = CcidHeader::parse(&built).unwrap();
        assert_eq!(parsed.length, 261);
    }

    #[test]
    fn test_header_length_exceeds_max_ccid_message() {
        let built = CcidHeader::build(0x80, 272, 0, 0, 0, 0, 0);
        let parsed = CcidHeader::parse(&built).unwrap();
        assert_eq!(parsed.length, 272);
    }

    #[test]
    fn test_header_length_one_byte() {
        let built = CcidHeader::build(0x6F, 1, 0, 0, 0, 0, 0);
        let parsed = CcidHeader::parse(&built).unwrap();
        assert_eq!(parsed.length, 1);
    }

    #[test]
    fn test_header_length_260_bytes() {
        let built = CcidHeader::build(0x6F, 260, 0, 0, 0, 0, 0);
        let parsed = CcidHeader::parse(&built).unwrap();
        assert_eq!(parsed.length, 260);
    }

    #[test]
    fn test_max_ccid_message_length_constant() {
        assert_eq!(MAX_CCID_MESSAGE_LENGTH, 271);
    }

    #[test]
    fn test_ccid_header_size_constant() {
        assert_eq!(CCID_HEADER_SIZE, 10);
    }
}
