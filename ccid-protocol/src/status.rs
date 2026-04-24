use crate::types::{COMMAND_STATUS_FAILED, COMMAND_STATUS_NO_ERROR, COMMAND_STATUS_TIME_EXTENSION};

/// Build a bStatus byte from command status and ICC status per CCID Rev 1.1 §4.2.2.
///
/// Encoding: `(cmd_status << 6) | icc_status`
/// - cmd_status: 2-bit value in bits [7:6]
/// - icc_status: 2-bit value in bits [1:0]
#[inline]
pub fn build_bstatus(cmd_status: u8, icc_status: u8) -> u8 {
    ((cmd_status & 0x03) << 6) | (icc_status & 0x03)
}

#[inline]
pub fn slot_status_ok(icc_status: u8) -> u8 {
    build_bstatus(COMMAND_STATUS_NO_ERROR, icc_status)
}

#[inline]
pub fn slot_status_failed(icc_status: u8) -> u8 {
    build_bstatus(COMMAND_STATUS_FAILED, icc_status)
}

#[inline]
pub fn slot_status_time_ext(icc_status: u8) -> u8 {
    build_bstatus(COMMAND_STATUS_TIME_EXTENSION, icc_status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ICC_STATUS_NO_ICC, ICC_STATUS_PRESENT_ACTIVE, ICC_STATUS_PRESENT_INACTIVE};

    #[test]
    fn test_build_bstatus() {
        assert_eq!(
            build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE),
            0x00
        );
        assert_eq!(
            build_bstatus(COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC),
            0x42
        );
        assert_eq!(
            build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_INACTIVE),
            0x01
        );
        assert_eq!(
            build_bstatus(COMMAND_STATUS_TIME_EXTENSION, ICC_STATUS_PRESENT_ACTIVE),
            0x80
        );
    }

    #[test]
    fn test_shorthand_helpers() {
        assert_eq!(slot_status_ok(ICC_STATUS_PRESENT_ACTIVE), 0x00);
        assert_eq!(slot_status_failed(ICC_STATUS_NO_ICC), 0x42);
        assert_eq!(slot_status_time_ext(ICC_STATUS_PRESENT_ACTIVE), 0x80);
    }
}
