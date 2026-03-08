//! CCID Smartcard Reader Firmware Library
//!
//! This library provides the core CCID protocol implementation and PIN pad
//! functionality for the STM32F469-DISCO smartcard reader.
//!
//! Reference: CCID Rev 1.1 Spec (USB-IF DWG_Smart-Card_CCID_Rev110.pdf)
//! Reference: https://ccid.apdu.fr/ccid/section.html (PIN pad reader identities)

#![cfg_attr(not(test), no_std)]

pub mod pinpad;
pub mod protocol_unit;

#[cfg(feature = "display")]
pub use pinpad::{draw_pinpad, ButtonId, Keypad, TouchHandler};
pub use pinpad::{ApduError, VerifyApduBuilder, VerifyResponse};
pub use pinpad::{PinBuffer, PinEntryContext, PinEntryState, PinResult, PinVerifyParams};

// ============================================================================
// CCID Class Functional Descriptor (52 bytes)
// ============================================================================
// Reference: CCID Rev 1.1 Spec Table 5.1-1
// NOTE: Array indices are 2 less than spec offsets (bLength/bDescriptorType
//       prepended by USB library, making our index 0 = spec offset 2)
// Total descriptor size: 54 bytes (52 + 2 byte header)
pub const CCID_CLASS_DESCRIPTOR_DATA: [u8; 52] = [
    // [0-1]   bcdCCID = 0x0110 (Rev 1.1, LE)
    0x10, 0x01, // [2]     bMaxSlotIndex = 0
    0x00, // [3]     bVoltageSupport = 0x07 (5V|3V|1.8V)
    0x07, // [4-7]   dwProtocols = 3 (T=0 | T=1)
    0x03, 0x00, 0x00, 0x00, // [8-11]  dwDefaultClock = 4 MHz (0x003D2D00)
    0x00, 0x2D, 0x3D, 0x00, // [12-15] dwMaximumClock = 20 MHz (0x01318480)
    0x80, 0x84, 0x31, 0x01, // [16]    bNumClockSupported = 0
    0x00, // [17-20] dwDataRate = 10752 bps (0x00002A00)
    0x00, 0x2A, 0x00, 0x00, // [21-24] dwMaxDataRate = 344086 bps (0x00054136)
    0x36, 0x41, 0x05, 0x00, // [25]    bNumDataRatesSupported = 0
    0x00, // [26-29] dwMaxIFSD = 254 (0xFE)
    0xFE, 0x00, 0x00, 0x00, // [30-33] dwSynchProtocols = 0
    0x00, 0x00, 0x00, 0x00, // [34-37] dwMechanical = 0
    0x00, 0x00, 0x00, 0x00,
    // [38-41] dwFeatures = 0x000100BA (matching Cherry ST-2100)
    //   Bit  2: Automatic activation of ICC on inserting
    //   Bit  4: Automatic baud rate change
    //   Bit  5: Automatic parameters negotiation
    //   Bit  7: Clock stop mode
    //   Bit  8: NAD value other than 0x00 accepted
    //   Bit 17: Short APDU level
    0xBA, 0x00, 0x01, 0x00,
    // [42-45] dwMaxCCIDMessageLength = 270 (0x010E)
    0x0E, 0x01, 0x00, 0x00,
    // [46]    bClassGetResponse = 0xFF (auto)
    0xFF,
    0xFF, // [48-49] wLcdLayout = 0 (disabled)
    0x00, 0x00, // [50]    bPINSupport = 0x00 (disabled for testing)
    0x00, // [51]    bMaxCCIDBusySlots = 1
    0x01,
];

// ============================================================================
// Unit Tests - CCID Rev 1.1 Spec Table 5.1-1
// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_descriptor_size() {
        assert_eq!(CCID_CLASS_DESCRIPTOR_DATA.len(), 52);
    }

    #[test]
    fn test_descriptor_bcd_ccid() {
        // Spec: bcdCCID at offset 2-3 = 0x0110 (Rev 1.1)
        assert_eq!(CCID_CLASS_DESCRIPTOR_DATA[0], 0x10);
        assert_eq!(CCID_CLASS_DESCRIPTOR_DATA[1], 0x01);
    }

    #[test]
    fn test_descriptor_max_slot() {
        // Spec: bMaxSlotIndex at offset 4 = 0 (single slot)
        assert_eq!(CCID_CLASS_DESCRIPTOR_DATA[2], 0x00);
    }

    #[test]
    fn test_descriptor_voltage_support() {
        // Spec: bVoltageSupport at offset 5 = 0x07 (5V|3V|1.8V)
        assert_eq!(CCID_CLASS_DESCRIPTOR_DATA[3], 0x07);
    }

    #[test]
    fn test_descriptor_protocols() {
        // Spec: dwProtocols at offset 6-9 = 3 (T=0 | T=1)
        let protocols = u32::from_le_bytes([
            CCID_CLASS_DESCRIPTOR_DATA[4],
            CCID_CLASS_DESCRIPTOR_DATA[5],
            CCID_CLASS_DESCRIPTOR_DATA[6],
            CCID_CLASS_DESCRIPTOR_DATA[7],
        ]);
        assert_eq!(protocols, 3);
    }

    #[test]
    fn test_descriptor_features() {
        // Spec: dwFeatures at offset 40-43
        let features = u32::from_le_bytes([
            CCID_CLASS_DESCRIPTOR_DATA[38],
            CCID_CLASS_DESCRIPTOR_DATA[39],
            CCID_CLASS_DESCRIPTOR_DATA[40],
            CCID_CLASS_DESCRIPTOR_DATA[41],
        ]);
        // Bit 17: Short APDU level (bit 18 LCD and bit 20 Keypad DISABLED)
        assert_eq!(features, 0x000107B0);
    }

    #[test]
    fn test_descriptor_max_message_length() {
        // Spec: dwMaxCCIDMessageLength at offset 44-47 = 271
        let len = u32::from_le_bytes([
            CCID_CLASS_DESCRIPTOR_DATA[42],
            CCID_CLASS_DESCRIPTOR_DATA[43],
            CCID_CLASS_DESCRIPTOR_DATA[44],
            CCID_CLASS_DESCRIPTOR_DATA[45],
        ]);
        assert_eq!(len, 271);
    }

    #[test]
    fn test_descriptor_pin_support() {
        // Spec: bPINSupport at offset 52 = 0x00 (disabled for testing)
        assert_eq!(CCID_CLASS_DESCRIPTOR_DATA[50], 0x00);
    }

    #[test]
    fn test_descriptor_lcd_layout() {
        // Spec: wLcdLayout at offset 50-51 = 0x0000 (disabled)
        assert_eq!(CCID_CLASS_DESCRIPTOR_DATA[48], 0x00);
        assert_eq!(CCID_CLASS_DESCRIPTOR_DATA[49], 0x00);
    }
}
