#![no_std]
#![allow(clippy::too_many_arguments)]

pub use card_interface::{CardBackend, ContactCardExt, NfcCardExt, PresenceState};
pub use ccid_protocol::*;

use ccid_protocol::status::build_bstatus;
use ccid_protocol::types::{
    CcidHeader, COMMAND_STATUS_NO_ERROR, DEFAULT_T0_PARAMS, DEFAULT_T1_PARAMS,
    ICC_STATUS_PRESENT_ACTIVE, RDR_TO_PC_DATABLOCK, RDR_TO_PC_PARAMETERS, RDR_TO_PC_SLOTSTATUS,
};

pub mod response {
    use super::*;

    pub fn write_slot_status(
        slot: u8,
        seq: u8,
        icc_status: u8,
        cmd_status: u8,
        error: u8,
        clock_status: u8,
        buf: &mut [u8],
    ) -> usize {
        write_message(
            RDR_TO_PC_SLOTSTATUS,
            slot,
            seq,
            build_bstatus(cmd_status, icc_status),
            error,
            clock_status,
            &[],
            buf,
        )
    }

    pub fn write_data_block(
        slot: u8,
        seq: u8,
        icc_status: u8,
        cmd_status: u8,
        error: u8,
        chain_parameter: u8,
        data: &[u8],
        buf: &mut [u8],
    ) -> usize {
        write_message(
            RDR_TO_PC_DATABLOCK,
            slot,
            seq,
            build_bstatus(cmd_status, icc_status),
            error,
            chain_parameter,
            data,
            buf,
        )
    }

    pub fn write_parameters(
        slot: u8,
        seq: u8,
        icc_status: u8,
        cmd_status: u8,
        error: u8,
        protocol: u8,
        params: &[u8],
        buf: &mut [u8],
    ) -> usize {
        write_message(
            RDR_TO_PC_PARAMETERS,
            slot,
            seq,
            build_bstatus(cmd_status, icc_status),
            error,
            protocol,
            params,
            buf,
        )
    }

    pub fn write_message(
        msg_type: u8,
        slot: u8,
        seq: u8,
        status: u8,
        error: u8,
        specific: u8,
        payload: &[u8],
        buf: &mut [u8],
    ) -> usize {
        let total_len = ccid_protocol::types::CCID_HEADER_SIZE + payload.len();
        if buf.len() < total_len {
            return 0;
        }

        let header = CcidHeader::build(
            msg_type,
            payload.len() as u32,
            slot,
            seq,
            status,
            error,
            specific,
        );

        buf[..ccid_protocol::types::CCID_HEADER_SIZE].copy_from_slice(&header);
        buf[ccid_protocol::types::CCID_HEADER_SIZE..total_len].copy_from_slice(payload);
        total_len
    }
}

pub mod pps {
    use super::*;

    pub fn is_pps_request(apdu: &[u8]) -> bool {
        if !(3..=5).contains(&apdu.len()) {
            return false;
        }

        if apdu[0] != 0xFF {
            return false;
        }

        let pps0 = apdu[1];
        (pps0 & 0xE0) == 0x00 && pps0 != 0x00
    }

    pub fn build_pps_response(slot: u8, seq: u8, pps_data: &[u8], buf: &mut [u8]) -> usize {
        response::write_data_block(
            slot,
            seq,
            ICC_STATUS_PRESENT_ACTIVE,
            COMMAND_STATUS_NO_ERROR,
            0,
            0,
            pps_data,
            buf,
        )
    }
}

pub mod params {
    use super::*;

    pub fn default_params(protocol: u8) -> &'static [u8] {
        match protocol {
            0 => &DEFAULT_T0_PARAMS[..],
            1 => &DEFAULT_T1_PARAMS[..],
            _ => &DEFAULT_T1_PARAMS[..],
        }
    }

    pub fn validate_params_length(protocol: u8, length: u32) -> bool {
        match protocol {
            0 => length == 5,
            1 => length == 7,
            _ => false,
        }
    }

    pub fn protocol_from_specific(specific: &[u8]) -> u8 {
        specific[0]
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
    use super::params::{default_params, protocol_from_specific, validate_params_length};
    use super::pps::{build_pps_response, is_pps_request};
    use super::response::{
        write_data_block, write_message, write_parameters, write_slot_status,
    };
    use super::*;
    use ccid_protocol::types::{
        CCID_HEADER_SIZE, COMMAND_STATUS_FAILED, DEFAULT_T0_PARAMS, DEFAULT_T1_PARAMS,
        ICC_STATUS_PRESENT_INACTIVE,
    };

    fn parse_response(bytes: &[u8]) -> (CcidHeader, &[u8]) {
        let header = CcidHeader::parse(&bytes[..CCID_HEADER_SIZE]).unwrap();
        (header, &bytes[CCID_HEADER_SIZE..])
    }

    #[test]
    fn test_write_slot_status() {
        let mut buf = [0u8; CCID_HEADER_SIZE];

        let written = write_slot_status(
            1,
            2,
            ICC_STATUS_PRESENT_ACTIVE,
            COMMAND_STATUS_NO_ERROR,
            3,
            4,
            &mut buf,
        );

        let (header, payload) = parse_response(&buf[..written]);
        assert_eq!(written, CCID_HEADER_SIZE);
        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(header.length, 0);
        assert_eq!(header.slot, 1);
        assert_eq!(header.seq, 2);
        assert_eq!(header.specific, [build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE), 3, 4]);
        assert!(payload.is_empty());
    }

    #[test]
    fn test_write_slot_status_returns_len() {
        let mut buf = [0u8; 32];
        assert_eq!(
            write_slot_status(
                0,
                9,
                ICC_STATUS_PRESENT_INACTIVE,
                COMMAND_STATUS_FAILED,
                0xFE,
                0x01,
                &mut buf,
            ),
            CCID_HEADER_SIZE,
        );
    }

    #[test]
    fn test_write_data_block_empty() {
        let mut buf = [0u8; CCID_HEADER_SIZE];

        let written = write_data_block(
            0,
            7,
            ICC_STATUS_PRESENT_ACTIVE,
            COMMAND_STATUS_NO_ERROR,
            0,
            0,
            &[],
            &mut buf,
        );

        let (header, payload) = parse_response(&buf[..written]);
        assert_eq!(written, CCID_HEADER_SIZE);
        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(header.length, 0);
        assert_eq!(header.specific, [0, 0, 0]);
        assert!(payload.is_empty());
    }

    #[test]
    fn test_write_data_block_with_payload() {
        let data = [0x90, 0x00, 0x61, 0x10];
        let mut buf = [0u8; 32];

        let written = write_data_block(
            2,
            3,
            ICC_STATUS_PRESENT_ACTIVE,
            COMMAND_STATUS_NO_ERROR,
            0,
            0x55,
            &data,
            &mut buf,
        );

        let (header, payload) = parse_response(&buf[..written]);
        assert_eq!(written, CCID_HEADER_SIZE + data.len());
        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(header.length, data.len() as u32);
        assert_eq!(header.slot, 2);
        assert_eq!(header.seq, 3);
        assert_eq!(header.specific, [0, 0, 0x55]);
        assert_eq!(payload, data);
    }

    #[test]
    fn test_write_data_block_buffer_too_small() {
        let mut buf = [0u8; 11];
        assert_eq!(
            write_data_block(
                0,
                0,
                ICC_STATUS_PRESENT_ACTIVE,
                COMMAND_STATUS_NO_ERROR,
                0,
                0,
                &[1, 2],
                &mut buf,
            ),
            0,
        );
    }

    #[test]
    fn test_write_parameters_t0() {
        let mut buf = [0u8; 32];

        let written = write_parameters(
            0,
            1,
            ICC_STATUS_PRESENT_ACTIVE,
            COMMAND_STATUS_NO_ERROR,
            0,
            0,
            &DEFAULT_T0_PARAMS,
            &mut buf,
        );

        let (header, payload) = parse_response(&buf[..written]);
        assert_eq!(written, 15);
        assert_eq!(header.message_type, RDR_TO_PC_PARAMETERS);
        assert_eq!(header.length, 5);
        assert_eq!(header.specific, [0, 0, 0]);
        assert_eq!(payload, DEFAULT_T0_PARAMS);
    }

    #[test]
    fn test_write_parameters_t1() {
        let mut buf = [0u8; 32];

        let written = write_parameters(
            0,
            2,
            ICC_STATUS_PRESENT_ACTIVE,
            COMMAND_STATUS_NO_ERROR,
            0,
            1,
            &DEFAULT_T1_PARAMS,
            &mut buf,
        );

        let (header, payload) = parse_response(&buf[..written]);
        assert_eq!(written, 17);
        assert_eq!(header.message_type, RDR_TO_PC_PARAMETERS);
        assert_eq!(header.length, 7);
        assert_eq!(header.specific, [0, 0, 1]);
        assert_eq!(payload, DEFAULT_T1_PARAMS);
    }

    #[test]
    fn test_write_message_zero_payload() {
        let mut buf = [0u8; 16];

        let written = write_message(0x83, 4, 5, 6, 7, 8, &[], &mut buf);

        let (header, payload) = parse_response(&buf[..written]);
        assert_eq!(written, CCID_HEADER_SIZE);
        assert_eq!(header.message_type, 0x83);
        assert_eq!(header.length, 0);
        assert_eq!(header.slot, 4);
        assert_eq!(header.seq, 5);
        assert_eq!(header.specific, [6, 7, 8]);
        assert!(payload.is_empty());
    }

    #[test]
    fn test_is_pps_request_valid_3byte() {
        assert!(is_pps_request(&[0xFF, 0x10, 0x96]));
    }

    #[test]
    fn test_is_pps_request_valid_4byte() {
        assert!(is_pps_request(&[0xFF, 0x10, 0x96, 0x77]));
    }

    #[test]
    fn test_is_pps_request_not_ppss() {
        assert!(!is_pps_request(&[0x00, 0x10, 0x96]));
    }

    #[test]
    fn test_is_pps_request_too_short() {
        assert!(!is_pps_request(&[0xFF, 0x10]));
    }

    #[test]
    fn test_is_pps_request_pps0_zero() {
        assert!(!is_pps_request(&[0xFF, 0x00, 0x96]));
    }

    #[test]
    fn test_build_pps_response() {
        let pps_data = [0xFF, 0x10, 0x96];
        let mut buf = [0u8; 32];

        let written = build_pps_response(0, 1, &pps_data, &mut buf);

        let (header, payload) = parse_response(&buf[..written]);
        assert_eq!(written, CCID_HEADER_SIZE + pps_data.len());
        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(header.length, pps_data.len() as u32);
        assert_eq!(header.specific, [0, 0, 0]);
        assert_eq!(payload, pps_data);
    }

    #[test]
    fn test_build_pps_response_buffer_too_small() {
        let mut buf = [0u8; 12];
        assert_eq!(build_pps_response(0, 0, &[0xFF, 0x10, 0x96], &mut buf), 0);
    }

    #[test]
    fn test_default_params_t0() {
        let params = default_params(0);
        assert_eq!(params.len(), 5);
        assert_eq!(params[0], 0x11);
    }

    #[test]
    fn test_default_params_t1() {
        let params = default_params(1);
        assert_eq!(params.len(), 7);
        assert_eq!(params, DEFAULT_T1_PARAMS);
    }

    #[test]
    fn test_validate_params_length_t0_valid() {
        assert!(validate_params_length(0, 5));
    }

    #[test]
    fn test_validate_params_length_t0_invalid() {
        assert!(!validate_params_length(0, 4));
    }

    #[test]
    fn test_validate_params_length_t1_valid() {
        assert!(validate_params_length(1, 7));
    }

    #[test]
    fn test_protocol_from_specific() {
        assert_eq!(protocol_from_specific(&[1, 2, 3]), 1);
    }
}
