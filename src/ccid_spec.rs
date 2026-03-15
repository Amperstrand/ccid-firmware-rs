//! CCID Specification Constants
//!
//! Reference: USB Chip/Smart Card Interface Devices (CCID) Rev 1.1
//! https://www.usb.org/document-library/smart-card-ccid-version-11
//!
//! Cross-reference: libccid ccid.h (authoritative open-source implementation)
//! https://github.com/LudovicRousseau/CCID/blob/master/src/ccid.h
//!
//! Reader database (empirical data from 262+ real readers):
//! https://ccid.apdu.fr/ccid/section.html

#![cfg(all(target_arch = "arm", target_os = "none"))]
#![allow(dead_code)]

// ============================================================================
// dwFeatures Bit Definitions (CCID Rev 1.1 Spec Table 5.1-1)
// ============================================================================
//
// These constants are copied verbatim from libccid ccid.h with added
// documentation. The libccid implementation is the de-facto reference
// for CCID on Linux/macOS.
//
// Ref: https://github.com/LudovicRousseau/CCID/blob/master/src/ccid.h

/// Bit 1: Automatic parameter configuration based on ATR data
/// libccid: CCID_CLASS_AUTO_CONF_ATR
pub const FEAT_AUTO_CONF_ATR: u32 = 0x0000_0002;

/// Bit 2: Automatic activation of ICC on inserting
/// libccid: CCID_CLASS_AUTO_ACTIVATION
pub const FEAT_AUTO_ACTIVATION: u32 = 0x0000_0004;

/// Bit 3: Automatic ICC voltage selection
/// libccid: CCID_CLASS_AUTO_VOLTAGE
pub const FEAT_AUTO_VOLTAGE: u32 = 0x0000_0008;

/// Bit 4: Automatic ICC clock frequency change
/// (Not defined in libccid ccid.h but present in spec and real readers)
pub const FEAT_AUTO_CLOCK: u32 = 0x0000_0010;

/// Bit 5: Automatic baud rate change
/// libccid: CCID_CLASS_AUTO_BAUD
pub const FEAT_AUTO_BAUD: u32 = 0x0000_0020;

/// Bit 6: Automatic parameters negotiation made by the CCID
/// libccid: CCID_CLASS_AUTO_PPS_PROP
pub const FEAT_AUTO_PPS_NEG: u32 = 0x0000_0040;

/// Bit 7: Automatic PPS made by the CCID according to active parameters
/// libccid: CCID_CLASS_AUTO_PPS_CUR
pub const FEAT_AUTO_PPS: u32 = 0x0000_0080;

/// Bit 8: CCID can set ICC in clock stop mode
/// (Present in spec, used by Cherry ST-2xxx: 0x000100BA includes 0x100)
pub const FEAT_CLOCK_STOP: u32 = 0x0000_0100;

/// Bit 9: NAD value other than 00 accepted (T=1 protocol in use)
/// (Present in spec, not in libccid constants)
pub const FEAT_NAD_OTHER: u32 = 0x0000_0200;

/// Bit 10: Automatic IFSD exchange as first exchange (T=1 protocol)
/// libccid: CCID_CLASS_AUTO_IFSD
pub const FEAT_AUTO_IFSD: u32 = 0x0000_0400;

// ============================================================================
// Exchange Level (mutually exclusive - only ONE should be set)
// ============================================================================

/// Character level exchange (no bits set = character level)
/// libccid: CCID_CLASS_CHARACTER
pub const FEAT_LEVEL_CHARACTER: u32 = 0x0000_0000;

/// Bit 16: TPDU level exchanges with CCID
/// libccid: CCID_CLASS_TPDU
pub const FEAT_LEVEL_TPDU: u32 = 0x0001_0000;

/// Bit 17: Short APDU level exchange with CCID
/// libccid: CCID_CLASS_SHORT_APDU
pub const FEAT_LEVEL_SHORT_APDU: u32 = 0x0002_0000;

/// Bit 18: Short and Extended APDU level exchange with CCID
/// libccid: CCID_CLASS_EXTENDED_APDU
pub const FEAT_LEVEL_EXTENDED_APDU: u32 = 0x0004_0000;

/// Mask for exchange level bits
/// libccid: CCID_CLASS_EXCHANGE_MASK
pub const FEAT_LEVEL_MASK: u32 = 0x0007_0000;

// ============================================================================
// IMPORTANT: LCD and PIN Pad are NOT dwFeatures bits!
// ============================================================================
//
// Contrary to what you might expect, there are NO bits for LCD or PIN pad
// in dwFeatures per the CCID spec. These capabilities are indicated by:
//
// - wLcdLayout: Non-zero value indicates LCD present (e.g., 0x0414 = 4 lines, 20 chars)
// - bPINSupport: Byte at offset 52 indicates PIN verify/modify support
//
// Some older/incorrect implementations may use bits 20-21, but these are
// NOT defined in the spec and may cause "Feature not supported" errors.

/// Bit 20: USB Wake up signaling (NOT LCD!)
/// Setting this incorrectly will cause PIN operations to fail
pub const FEAT_USB_WAKEUP: u32 = 0x0010_0000;

// ============================================================================
// bPINSupport Flags (separate field, NOT part of dwFeatures)
// ============================================================================
//
// Ref: CCID Rev 1.1 Spec, offset 52 in class descriptor
// Ref: libccid ccid.h: CCID_CLASS_PIN_VERIFY, CCID_CLASS_PIN_MODIFY

/// PIN verification supported (bPINSupport bit 0)
/// libccid: CCID_CLASS_PIN_VERIFY
pub const PIN_VERIFY: u8 = 0x01;

/// PIN modification supported (bPINSupport bit 1)
/// libccid: CCID_CLASS_PIN_MODIFY
pub const PIN_MODIFY: u8 = 0x02;

/// Both verification and modification supported
pub const PIN_VERIFY_MODIFY: u8 = PIN_VERIFY | PIN_MODIFY;

// ============================================================================
// ICC Status (bStatus field in responses)
// ============================================================================
//
// Ref: CCID Rev 1.1 Spec ch. 4.2.1
// Ref: libccid ccid.h

/// ICC present and active
pub const ICC_STATUS_PRESENT_ACTIVE: u8 = 0x00;

/// ICC present but inactive (not powered on)
pub const ICC_STATUS_PRESENT_INACTIVE: u8 = 0x01;

/// No ICC present
pub const ICC_STATUS_NO_ICC: u8 = 0x02;

/// Mask for ICC status bits
pub const ICC_STATUS_MASK: u8 = 0x03;

// ============================================================================
// Command Status (bStatus field in responses)
// ============================================================================

/// Command failed
pub const COMMAND_STATUS_FAILED: u8 = 0x40;

/// Time extension requested
pub const COMMAND_STATUS_TIME_EXTENSION: u8 = 0x80;

/// Command succeeded (no error)
pub const COMMAND_STATUS_NO_ERROR: u8 = 0x00;

// ============================================================================
// CCID Message Types
// ============================================================================
//
// Ref: CCID Rev 1.1 Spec
// Ref: libccid ccid.h

// Command Pipe (Bulk-OUT)
pub const PC_TO_RDR_ICC_POWER_ON: u8 = 0x62;
pub const PC_TO_RDR_ICC_POWER_OFF: u8 = 0x63;
pub const PC_TO_RDR_GET_SLOT_STATUS: u8 = 0x65;
pub const PC_TO_RDR_XFR_BLOCK: u8 = 0x6F;
pub const PC_TO_RDR_GET_PARAMETERS: u8 = 0x6C;
pub const PC_TO_RDR_RESET_PARAMETERS: u8 = 0x6D;
pub const PC_TO_RDR_SET_PARAMETERS: u8 = 0x61;
pub const PC_TO_RDR_ESCAPE: u8 = 0x6B;
pub const PC_TO_RDR_ICC_CLOCK: u8 = 0x6E;
pub const PC_TO_RDR_T0_APDU: u8 = 0x6A;
pub const PC_TO_RDR_SECURE: u8 = 0x69;
pub const PC_TO_RDR_MECHANICAL: u8 = 0x71;
pub const PC_TO_RDR_ABORT: u8 = 0x72;
pub const PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ: u8 = 0x73;

// Response Pipe (Bulk-IN)
pub const RDR_TO_PC_DATA_BLOCK: u8 = 0x80;
pub const RDR_TO_PC_SLOT_STATUS: u8 = 0x81;
pub const RDR_TO_PC_PARAMETERS: u8 = 0x82;
pub const RDR_TO_PC_ESCAPE: u8 = 0x83;
pub const RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ: u8 = 0x84;

// Interrupt-IN
pub const RDR_TO_PC_NOTIFY_SLOT_CHANGE: u8 = 0x50;
pub const RDR_TO_PC_HARDWARE_ERROR: u8 = 0x51;

// ============================================================================
// CCID Error Codes
// ============================================================================

pub const CCID_ERR_CMD_NOT_SUPPORTED: u8 = 0x00;
pub const CCID_ERR_CMD_ABORTED: u8 = 0x01;
pub const CCID_ERR_ICC_MUTE: u8 = 0x02;
pub const CCID_ERR_XFR_PARITY_ERROR: u8 = 0x03;
pub const CCID_ERR_XFR_OVERRUN: u8 = 0x04;
pub const CCID_ERR_HW_ERROR: u8 = 0x05;
pub const CCID_ERR_BAD_ATR: u8 = 0x06;
pub const CCID_ERR_PARITY_ERROR: u8 = 0x07;
pub const CCID_ERR_PROCEDURE_BYTE_CONFLICT: u8 = 0x08;
pub const CCID_ERR_DEACTIVATED_PROTOCOL: u8 = 0x09;
pub const CCID_ERR_BUSY_WITH_AUTO_SEQUENCE: u8 = 0x0A;
pub const CCID_ERR_PIN_TIMEOUT: u8 = 0x0B;
pub const CCID_ERR_PIN_CANCELLED: u8 = 0x0C;
pub const CCID_ERR_CMD_SLOT_BUSY: u8 = 0xE0;

// ============================================================================
// Reference Reader Profiles (from ccid.apdu.fr)
// ============================================================================
//
// These are empirically verified dwFeatures values from real readers.
// Use these as authoritative references when creating new profiles.

/// Cherry SmartTerminal ST-2xxx (VID:046A PID:003E)
/// Ref: https://ccid.apdu.fr/ccid/supported.html#0x046A0x003E
///
/// dwFeatures = 0x000100BA
/// Decoded: AUTO_CONF_ATR | AUTO_VOLTAGE | AUTO_CLOCK | AUTO_BAUD | AUTO_PPS | CLOCK_STOP | TPDU
/// bPINSupport = 0x03 (verify + modify)
/// wLcdLayout = 0x0000 (no LCD in descriptor, but reader has one)
pub const CHERRY_ST2XXX_DWFEATURES: u32 = 0x0001_00BA;

/// Gemalto IDBridge CT30 (VID:08E6 PID:3437)
/// Ref: reference/CCID/readers/Gemalto_IDBridge_CT30.txt
///
/// dwFeatures = 0x00010230
/// Decoded: AUTO_CLOCK | AUTO_BAUD | NAD_OTHER | TPDU
/// bPINSupport = 0x00 (no PIN pad)
/// wLcdLayout = 0x0000 (no LCD)
pub const GEMALTO_CT30_DWFEATURES: u32 = 0x0001_0230;

/// Gemalto IDBridge K30 (VID:08E6 PID:3438)
/// Ref: reference/CCID/readers/Gemalto_IDBridge_K30.txt
///
/// dwFeatures = 0x00010230 (TPDU level - SAME AS CT30!)
/// Decoded: AUTO_CLOCK | AUTO_BAUD | NAD_OTHER | TPDU
///
/// ⚠️ IMPORTANT: The K30 is NOT a PIN pad reader!
/// - bPINSupport = 0x00 (NO PIN PAD!)
/// - wLcdLayout = 0x0000 (NO LCD!)
///
/// For PIN pad support, use Cherry ST-2xxx profile instead.
pub const GEMALTO_K30_DWFEATURES: u32 = 0x0001_0230;

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cherry_st2xxx_features() {
        // Verify our decomposition of Cherry ST-2xxx dwFeatures
        let expected = CHERRY_ST2XXX_DWFEATURES;
        let computed = FEAT_AUTO_CONF_ATR
            | FEAT_AUTO_VOLTAGE
            | FEAT_AUTO_CLOCK
            | FEAT_AUTO_BAUD
            | FEAT_AUTO_PPS
            | FEAT_CLOCK_STOP
            | FEAT_LEVEL_TPDU;
        assert_eq!(expected, computed, "Cherry ST-2xxx dwFeatures mismatch");
    }

    #[test]
    fn test_gemalto_ct30_features() {
        let expected = GEMALTO_CT30_DWFEATURES;
        // 0x00010230 = AUTO_CLOCK | AUTO_BAUD | NAD_OTHER | TPDU
        let computed = FEAT_AUTO_CLOCK | FEAT_AUTO_BAUD | FEAT_NAD_OTHER | FEAT_LEVEL_TPDU;
        assert_eq!(expected, computed, "Gemalto CT30 dwFeatures mismatch");
    }

    #[test]
    fn test_gemalto_k30_features() {
        let expected = GEMALTO_K30_DWFEATURES;
        let computed = FEAT_AUTO_CLOCK | FEAT_AUTO_BAUD | FEAT_NAD_OTHER | FEAT_LEVEL_TPDU;
        assert_eq!(expected, computed, "Gemalto K30 dwFeatures mismatch");
    }

    #[test]
    fn test_exchange_level_mutually_exclusive() {
        // Only one exchange level should be set at a time
        let cherry_level = CHERRY_ST2XXX_DWFEATURES & FEAT_LEVEL_MASK;
        assert_eq!(
            cherry_level, FEAT_LEVEL_TPDU,
            "Cherry should use TPDU level"
        );

        let ct30_level = GEMALTO_CT30_DWFEATURES & FEAT_LEVEL_MASK;
        assert_eq!(ct30_level, FEAT_LEVEL_TPDU, "CT30 should use TPDU level");

        let k30_level = GEMALTO_K30_DWFEATURES & FEAT_LEVEL_MASK;
        assert_eq!(
            k30_level, FEAT_LEVEL_TPDU,
            "K30 should use TPDU level (same as CT30)"
        );
    }
}
