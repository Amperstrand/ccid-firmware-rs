//! CCID Smartcard Reader Firmware Library
//!
//! This library provides the core CCID protocol implementation and PIN pad
//! functionality for the STM32F469-DISCO smartcard reader.
//!
//! Reference: CCID Rev 1.1 Spec (USB-IF DWG_Smart-Card_CCID_Rev110.pdf)
//! Reference: https://ccid.apdu.fr/ccid/section.html (PIN pad reader identities)

#![no_std]
#![allow(dead_code)] // PIN pad scaffolding not yet in use
#![allow(unused_imports)] // Public re-exports for future use
#![allow(clippy::identity_op)] // XOR with 0 for LRC clarity
#![allow(clippy::manual_div_ceil)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::manual_is_multiple_of)]

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
    0xBA, 0x00, 0x01, 0x00, // [42-45] dwMaxCCIDMessageLength = 270 (0x010E)
    0x0E, 0x01, 0x00, 0x00, // [46]    bClassGetResponse = 0xFF (auto)
    0xFF, 0xFF, // [48-49] wLcdLayout = 0 (disabled)
    0x00, 0x00, // [50]    bPINSupport = 0x00 (disabled for testing)
    0x00, // [51]    bMaxCCIDBusySlots = 1
    0x01,
];
