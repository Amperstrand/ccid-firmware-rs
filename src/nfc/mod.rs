//! NFC backend for CCID firmware
//!
//! This module provides an NFC-based smartcard reader using the PN532 NFC
//! controller. Instead of the ISO 7816 contact-card UART interface used
//! by the STM32 backend, this module communicates with ISO 14443-4
//! (ISO-DEP) contactless cards via the PN532 over SPI.
//!
//! # Architecture
//!
//! ```text
//! Host PC (USB CCID) → ESP32-S3 (this firmware) → PN532 (SPI) → NFC Card
//! ```
//!
//! The [`Pn532Driver`] implements the [`SmartcardDriver`](crate::driver::SmartcardDriver)
//! trait, allowing it to plug into the existing CCID core
//! ([`CcidMessageHandler`](crate::ccid_core::CcidMessageHandler)) with no
//! changes to the protocol handling layer.
//!
//! # Supported Card Types (v1)
//!
//! - ISO 14443-4 Type A (ISO-DEP) — APDU-capable contactless cards
//!   (JavaCard, JCOP, YubiKey NFC, etc.)
//!
//! # Usage
//!
//! ```ignore
//! // Construct with an SPI bus and CS pin (embedded-hal 0.2 traits)
//! let pn532 = Pn532::new(spi, cs_pin);
//! let driver = Pn532Driver::new(pn532);
//!
//! // Use with CCID core exactly like the STM32 contact-card driver
//! let handler = CcidMessageHandler::new(driver, vendor_id);
//! ```

mod pn532;
mod pn532_driver;

pub use pn532::{Pn532, Pn532Error};
pub use pn532_driver::Pn532Driver;

use crate::device_profile::{
    DeviceProfile, ExchangeLevel, FEAT_AUTO_ACTIVATE, FEAT_AUTO_BAUD, FEAT_AUTO_CLOCK,
    FEAT_AUTO_PARAM_ATR, FEAT_AUTO_PPS, FEAT_AUTO_VOLTAGE, FEAT_LEVEL_SHORT_APDU,
};

/// NFC CCID device profile for a contactless reader (ESP32-S3 + PN532).
///
/// This profile advertises Short APDU level exchange — the host sends raw
/// APDUs and the reader handles all NFC/ISO-DEP framing internally via
/// the PN532.
///
/// Protocol is set to T=1 (bit 1 of dwProtocols) because ISO-DEP is
/// structurally closest to T=1, and PC/SC drivers expect T=1 for
/// contactless readers (the synthetic ATR also indicates T=1).
///
/// The USB VID:PID uses a testing/prototype value. For production use,
/// replace with a properly allocated VID:PID.
pub const NFC_PROFILE: DeviceProfile = DeviceProfile {
    // USB Identity — prototype / testing values
    // TODO: Replace with a properly allocated VID:PID for production
    vendor_id: 0x1209,  // pid.codes VID (open-source hardware)
    product_id: 0xCC1D, // "CCID" in hex-speak
    device_release: 0x0100,
    manufacturer: "ccid-firmware-rs",
    product: "NFC CCID Reader (PN532)",
    serial_number: "NFC-PN532-001",

    // CCID Version
    bcd_ccid: 0x0110, // Rev 1.1
    max_slot_index: 0, // single slot
    // Voltage support: report all (not physically meaningful for NFC,
    // but required to be non-zero for CCID compliance)
    voltage_support: 0x07, // 5V | 3V | 1.8V

    // Protocol: T=1 only (ISO-DEP maps to T=1 semantics)
    protocols: 0x02, // T=1

    // Timing: NFC carrier is 13.56 MHz, data rate 106 kbps (ISO 14443A)
    default_clock_khz: 13560,
    max_clock_khz: 13560,
    num_clocks: 1,
    default_data_rate: 106_000,
    max_data_rate: 848_000, // ISO 14443 4x mode
    num_data_rates: 1,

    // T=1 Parameters (used in CCID descriptor)
    max_ifsd: 254,
    synch_protocols: 0,
    mechanical: 0, // no mechanical features (no card insertion mechanism)

    // Features: Short APDU level, auto everything
    // The reader handles all NFC framing; host sends/receives raw APDUs.
    features: FEAT_AUTO_PARAM_ATR
        | FEAT_AUTO_ACTIVATE
        | FEAT_AUTO_VOLTAGE
        | FEAT_AUTO_CLOCK
        | FEAT_AUTO_BAUD
        | FEAT_AUTO_PPS
        | FEAT_LEVEL_SHORT_APDU,

    // Message Size
    max_ccid_message_length: 271,

    // Class Bytes
    class_get_response: 0xFF,
    class_envelope: 0xFF,

    // No display or PIN pad on this reader variant
    lcd_layout: (0, 0),
    pin_support: 0x00,

    // Concurrency
    max_busy_slots: 1,

    // USB Interface: Standard CCID class
    interface_class: 0x0B,

    // Exchange level: Short APDU (host sends raw APDUs)
    exchange_level: ExchangeLevel::ShortApdu,
};

// ============================================================================
// Synthetic ATR for contactless cards (PC/SC Part 3)
// ============================================================================

/// Maximum length of a synthetic ATR
pub const MAX_SYNTHETIC_ATR_LEN: usize = 21;

/// Build a PC/SC Part 3 compliant synthetic ATR for an ISO 14443-4 Type A
/// contactless card.
///
/// The ATR encodes the card's ATQA and SAK into a standardized format that
/// PC/SC drivers and middleware understand. This allows contactless cards
/// to be treated identically to contact cards by host software.
///
/// # Format (20 bytes + TCK)
///
/// ```text
/// 3B 8F 80 01 80 4F 0C A0 00 00 03 06 03 [ATQA₁] [ATQA₂] [SAK] 00 00 00 00 [TCK]
/// ```
///
/// - `3B`: TS — Direct convention
/// - `8F`: T0 — TD1 present, 15 historical bytes
/// - `80`: TD1 — TD2 present, T=0
/// - `01`: TD2 — T=1 (preferred protocol for contactless)
/// - `80 4F 0C`: Historical bytes category indicator
/// - `A0 00 00 03 06`: PC/SC RID (Registered Application Provider Identifier)
/// - `03`: Card standard — ISO 14443A Part 3
/// - ATQA₁, ATQA₂: Card's ATQA bytes (SENS_RES)
/// - SAK: Card's SAK byte (SEL_RES)
/// - `00 00 00 00`: RFU (Reserved for Future Use)
/// - TCK: Check byte (XOR of T0 through last historical byte)
///
/// # Returns
///
/// Number of bytes written to `atr_buf` (always 21).
pub fn build_synthetic_atr(atqa: [u8; 2], sak: u8, atr_buf: &mut [u8; MAX_SYNTHETIC_ATR_LEN]) -> usize {
    // Fixed ATR template (bytes 0-12)
    atr_buf[0] = 0x3B; // TS: Direct convention
    atr_buf[1] = 0x8F; // T0: TD1 present, K=15 (15 historical bytes)
    atr_buf[2] = 0x80; // TD1: TD2 present, T=0
    atr_buf[3] = 0x01; // TD2: T=1 (contactless uses T=1 semantics)

    // Historical bytes: Category indicator + PC/SC RID
    atr_buf[4] = 0x80; // Category indicator: status information
    atr_buf[5] = 0x4F; // Application identifier presence indicator
    atr_buf[6] = 0x0C; // Length of following data
    // PC/SC RID (Registered Application Provider Identifier)
    atr_buf[7] = 0xA0;
    atr_buf[8] = 0x00;
    atr_buf[9] = 0x00;
    atr_buf[10] = 0x03;
    atr_buf[11] = 0x06;
    // Card standard: ISO 14443A Part 3
    atr_buf[12] = 0x03;
    // Card name: ATQA bytes
    atr_buf[13] = atqa[0];
    atr_buf[14] = atqa[1];
    // SAK
    atr_buf[15] = sak;
    // RFU
    atr_buf[16] = 0x00;
    atr_buf[17] = 0x00;
    atr_buf[18] = 0x00;
    atr_buf[19] = 0x00;

    // TCK: XOR of bytes T0 through last historical byte (indices 1..=19)
    let mut tck: u8 = 0;
    let mut i = 1;
    while i <= 19 {
        tck ^= atr_buf[i];
        i += 1;
    }
    atr_buf[20] = tck;

    MAX_SYNTHETIC_ATR_LEN // always 21 bytes
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nfc_profile_descriptor_size() {
        let desc = NFC_PROFILE.ccid_descriptor();
        assert_eq!(desc.len(), 52);
    }

    #[test]
    fn test_nfc_profile_short_apdu_level() {
        assert!(NFC_PROFILE.is_short_apdu());
        assert_eq!(NFC_PROFILE.exchange_level, ExchangeLevel::ShortApdu);
    }

    #[test]
    fn test_nfc_profile_no_pin_pad() {
        assert!(!NFC_PROFILE.has_pin_pad());
        assert_eq!(NFC_PROFILE.pin_support, 0x00);
    }

    #[test]
    fn test_nfc_profile_protocols_t1_only() {
        let desc = NFC_PROFILE.ccid_descriptor();
        let protocols = u32::from_le_bytes([desc[4], desc[5], desc[6], desc[7]]);
        assert_eq!(protocols, 0x02); // T=1 only
    }

    #[test]
    fn test_nfc_profile_features_short_apdu() {
        let desc = NFC_PROFILE.ccid_descriptor();
        let features = u32::from_le_bytes([desc[38], desc[39], desc[40], desc[41]]);
        // Must have Short APDU level
        assert_ne!(features & FEAT_LEVEL_SHORT_APDU, 0);
        // Must have auto-activate
        assert_ne!(features & FEAT_AUTO_ACTIVATE, 0);
    }

    #[test]
    fn test_nfc_profile_clock_frequency() {
        let desc = NFC_PROFILE.ccid_descriptor();
        let clock_khz = u32::from_le_bytes([desc[8], desc[9], desc[10], desc[11]]);
        assert_eq!(clock_khz, 13560); // 13.56 MHz NFC carrier
    }

    #[test]
    fn test_synthetic_atr_format() {
        let mut atr = [0u8; MAX_SYNTHETIC_ATR_LEN];
        let len = build_synthetic_atr([0x00, 0x04], 0x20, &mut atr);

        assert_eq!(len, 21);
        assert_eq!(atr[0], 0x3B); // TS
        assert_eq!(atr[1], 0x8F); // T0
        assert_eq!(atr[2], 0x80); // TD1
        assert_eq!(atr[3], 0x01); // TD2 (T=1)
        // PC/SC RID
        assert_eq!(atr[7], 0xA0);
        assert_eq!(atr[11], 0x06);
        // Card info
        assert_eq!(atr[12], 0x03); // ISO 14443A-3
        assert_eq!(atr[13], 0x00); // ATQA[0]
        assert_eq!(atr[14], 0x04); // ATQA[1]
        assert_eq!(atr[15], 0x20); // SAK
    }

    #[test]
    fn test_synthetic_atr_tck_checksum() {
        let mut atr = [0u8; MAX_SYNTHETIC_ATR_LEN];
        build_synthetic_atr([0x00, 0x04], 0x20, &mut atr);

        // TCK = XOR of bytes 1..=19
        let computed_tck = atr[1..=19].iter().fold(0u8, |acc, &b| acc ^ b);
        assert_eq!(atr[20], computed_tck);
    }

    #[test]
    fn test_synthetic_atr_known_value() {
        // Known ATR from ACR122U for ATQA=[0x00,0x01], SAK=0x00:
        // 3B 8F 80 01 80 4F 0C A0 00 00 03 06 03 00 01 00 00 00 00 00 6A
        let mut atr = [0u8; MAX_SYNTHETIC_ATR_LEN];
        build_synthetic_atr([0x00, 0x01], 0x00, &mut atr);

        // Verify TCK matches known value
        assert_eq!(atr[20], 0x6A);
    }
}
