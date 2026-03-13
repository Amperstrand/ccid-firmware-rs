#![cfg(all(target_arch = "arm", target_os = "none"))]
//! Device Profile Configuration for CCID Reader Emulation
//!
//! This module provides a unified configuration system for emulating different
//! CCID smartcard readers. Each profile defines the USB identity and CCID
//! capabilities that match a specific target device.
//!
//! # Available Profiles
//!
//! - `cherry-st2100`: Cherry SmartTerminal ST-2100 (PIN pad reader)
//! - `gemalto-plain`: Gemalto IDBridge CT30 (basic reader, no PIN pad)
//! - `gemalto-pinpad`: Gemalto IDBridge K30 (PIN pad reader)
//!
//! # Usage
//!
//! Select a profile at compile time via Cargo features:
//! ```bash
//! cargo build --features cherry-st2100
//! cargo build --features gemalto-plain
//! cargo build --features gemalto-pinpad
//! ```

// ============================================================================
// CCID Feature Bit Definitions (CCID Rev 1.1 Spec Table 5.1-1)
// ============================================================================

/// Bit 1: Automatic parameter configuration based on ATR
pub const FEAT_AUTO_PARAM_ATR: u32 = 0x0000_0002;
/// Bit 2: Automatic activation of ICC on inserting
pub const FEAT_AUTO_ACTIVATE: u32 = 0x0000_0004;
/// Bit 3: Automatic voltage selection
pub const FEAT_AUTO_VOLTAGE: u32 = 0x0000_0008;
/// Bit 4: Automatic ICC clock frequency change
pub const FEAT_AUTO_CLOCK: u32 = 0x0000_0010;
/// Bit 5: Automatic baud rate change
pub const FEAT_AUTO_BAUD: u32 = 0x0000_0020;
/// Bit 6: Automatic parameter negotiation
pub const FEAT_AUTO_PPS_NEG: u32 = 0x0000_0040;
/// Bit 7: Automatic PPS
pub const FEAT_AUTO_PPS: u32 = 0x0000_0080;
/// Bit 8: Clock stop mode supported
pub const FEAT_CLOCK_STOP: u32 = 0x0000_0100;
/// Bit 9: NAD value other than 0x00 accepted
pub const FEAT_NAD_OTHER: u32 = 0x0000_0200;
/// Bit 10: Automatic IFSD exchange (CAUTION: reader handles IFSD negotiation)
pub const FEAT_AUTO_IFSD: u32 = 0x0000_0400;
/// Bit 11: Exchange level: Character level (rarely used)
pub const FEAT_LEVEL_CHARACTER: u32 = 0x0000_0800;
/// Bit 12: Exchange level: TPDU level
pub const FEAT_LEVEL_TPDU: u32 = 0x0000_1000;
/// Bit 13: Exchange level: Short APDU level
pub const FEAT_LEVEL_SHORT_APDU: u32 = 0x0000_2000;
/// Bit 14: Exchange level: Extended APDU level
pub const FEAT_LEVEL_EXTENDED_APDU: u32 = 0x0000_4000;
/// Bit 16: TPDU level exchange (alternative bit position, older spec)
pub const FEAT_TPDU_LEVEL: u32 = 0x0001_0000;
/// Bit 17: Short APDU level exchange
pub const FEAT_SHORT_APDU_LEVEL: u32 = 0x0002_0000;
/// Bit 18: Extended APDU level exchange
pub const FEAT_EXTENDED_APDU_LEVEL: u32 = 0x0004_0000;
/// Bit 20: LCD display present (wLcdLayout valid)
pub const FEAT_LCD: u32 = 0x0010_0000;
/// Bit 21: PIN pad present
pub const FEAT_PIN_PAD: u32 = 0x0020_0000;
/// Bit 22: Keypad present
pub const FEAT_KEYPAD: u32 = 0x0040_0000;

// ============================================================================
// PIN Support Flags (bPINSupport)
// ============================================================================

/// PIN verification supported
pub const PIN_VERIFY: u8 = 0x01;
/// PIN modification supported
pub const PIN_MODIFY: u8 = 0x02;
/// Both verification and modification supported
pub const PIN_VERIFY_MODIFY: u8 = 0x03;

// ============================================================================
// Exchange Level Enumeration
// ============================================================================

/// CCID exchange level determines how APDUs are framed
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExchangeLevel {
    /// TPDU level: Host sends T=1 blocks, reader forwards to card
    /// Use `transmit_raw()` for T=1 framed data
    Tpdu,
    /// Short APDU level: Host sends raw APDUs, reader/libccid handles T=1 framing
    /// Use `transmit_apdu()` for raw APDU data
    ShortApdu,
    /// Extended APDU level: Supports APDUs > 255 bytes
    ExtendedApdu,
}

// ============================================================================
// Device Profile Structure
// ============================================================================

/// Complete configuration for a CCID reader device profile
///
/// This struct contains all parameters needed to generate USB descriptors
/// and CCID class descriptors that match a specific target device.
#[derive(Clone, Copy, Debug)]
pub struct DeviceProfile {
    // USB Device Descriptor
    /// USB Vendor ID
    pub vendor_id: u16,
    /// USB Product ID
    pub product_id: u16,
    /// USB device release number (BCD)
    pub device_release: u16,
    /// Manufacturer string (max 126 chars for USB)
    pub manufacturer: &'static str,
    /// Product string (max 126 chars for USB)
    pub product: &'static str,
    /// Serial number string
    pub serial_number: &'static str,

    // CCID Class Descriptor - Basic
    /// CCID spec version (0x0110 = Rev 1.1)
    pub bcd_ccid: u16,
    /// Maximum slot index (0 for single slot)
    pub max_slot_index: u8,
    /// Voltage support bitmask (5V|3V|1.8V = 0x07)
    pub voltage_support: u8,
    /// Supported protocols (T=0 = 1, T=1 = 2, both = 3)
    pub protocols: u32,

    // CCID Class Descriptor - Timing
    /// Default clock frequency in kHz
    pub default_clock_khz: u32,
    /// Maximum clock frequency in kHz
    pub max_clock_khz: u32,
    /// Number of supported clock frequencies (0 = continuous range)
    pub num_clocks: u8,
    /// Default data rate in bps
    pub default_data_rate: u32,
    /// Maximum data rate in bps
    pub max_data_rate: u32,
    /// Number of supported data rates (0 = continuous range)
    pub num_data_rates: u8,

    // CCID Class Descriptor - T=1 Parameters
    /// Maximum IFSD (Information Field Size for Device)
    pub max_ifsd: u32,
    /// Synchronous protocols supported (usually 0)
    pub synch_protocols: u32,
    /// Mechanical features (usually 0)
    pub mechanical: u32,

    // CCID Class Descriptor - Features (critical for libccid behavior)
    /// Feature flags (see FEAT_* constants)
    pub features: u32,

    // CCID Class Descriptor - Message Size
    /// Maximum CCID message length (header + data)
    pub max_ccid_message_length: u32,

    // CCID Class Descriptor - Class Bytes
    /// Class byte for GetResponse (0xFF = automatic)
    pub class_get_response: u8,
    /// Class byte for Envelope (0xFF = automatic)
    pub class_envelope: u8,

    // CCID Class Descriptor - Display/PIN
    /// LCD layout: [lines, chars_per_line] (0x0000 = no display)
    pub lcd_layout: (u8, u8),
    /// PIN support flags (see PIN_* constants)
    pub pin_support: u8,
    /// Maximum concurrent busy slots
    pub max_busy_slots: u8,

    // Firmware-specific settings
    /// Exchange level for APDU handling
    pub exchange_level: ExchangeLevel,
}

impl DeviceProfile {
    /// Generate the 52-byte CCID class descriptor data
    ///
    /// This produces the exact byte array expected by the USB CCID class
    /// implementation. The array is formatted per CCID Rev 1.1 spec Table 5.1-1.
    pub const fn ccid_descriptor(&self) -> [u8; 52] {
        let mut desc = [0u8; 52];

        // [0-1] bcdCCID (Little Endian)
        desc[0] = (self.bcd_ccid & 0xFF) as u8;
        desc[1] = ((self.bcd_ccid >> 8) & 0xFF) as u8;

        // [2] bMaxSlotIndex
        desc[2] = self.max_slot_index;

        // [3] bVoltageSupport
        desc[3] = self.voltage_support;

        // [4-7] dwProtocols
        desc[4] = (self.protocols & 0xFF) as u8;
        desc[5] = ((self.protocols >> 8) & 0xFF) as u8;
        desc[6] = ((self.protocols >> 16) & 0xFF) as u8;
        desc[7] = ((self.protocols >> 24) & 0xFF) as u8;

        // [8-11] dwDefaultClock
        desc[8] = (self.default_clock_khz & 0xFF) as u8;
        desc[9] = ((self.default_clock_khz >> 8) & 0xFF) as u8;
        desc[10] = ((self.default_clock_khz >> 16) & 0xFF) as u8;
        desc[11] = ((self.default_clock_khz >> 24) & 0xFF) as u8;

        // [12-15] dwMaximumClock
        desc[12] = (self.max_clock_khz & 0xFF) as u8;
        desc[13] = ((self.max_clock_khz >> 8) & 0xFF) as u8;
        desc[14] = ((self.max_clock_khz >> 16) & 0xFF) as u8;
        desc[15] = ((self.max_clock_khz >> 24) & 0xFF) as u8;

        // [16] bNumClockSupported
        desc[16] = self.num_clocks;

        // [17-20] dwDataRate
        desc[17] = (self.default_data_rate & 0xFF) as u8;
        desc[18] = ((self.default_data_rate >> 8) & 0xFF) as u8;
        desc[19] = ((self.default_data_rate >> 16) & 0xFF) as u8;
        desc[20] = ((self.default_data_rate >> 24) & 0xFF) as u8;

        // [21-24] dwMaxDataRate
        desc[21] = (self.max_data_rate & 0xFF) as u8;
        desc[22] = ((self.max_data_rate >> 8) & 0xFF) as u8;
        desc[23] = ((self.max_data_rate >> 16) & 0xFF) as u8;
        desc[24] = ((self.max_data_rate >> 24) & 0xFF) as u8;

        // [25] bNumDataRatesSupported
        desc[25] = self.num_data_rates;

        // [26-29] dwMaxIFSD
        desc[26] = (self.max_ifsd & 0xFF) as u8;
        desc[27] = ((self.max_ifsd >> 8) & 0xFF) as u8;
        desc[28] = ((self.max_ifsd >> 16) & 0xFF) as u8;
        desc[29] = ((self.max_ifsd >> 24) & 0xFF) as u8;

        // [30-33] dwSynchProtocols
        desc[30] = (self.synch_protocols & 0xFF) as u8;
        desc[31] = ((self.synch_protocols >> 8) & 0xFF) as u8;
        desc[32] = ((self.synch_protocols >> 16) & 0xFF) as u8;
        desc[33] = ((self.synch_protocols >> 24) & 0xFF) as u8;

        // [34-37] dwMechanical
        desc[34] = (self.mechanical & 0xFF) as u8;
        desc[35] = ((self.mechanical >> 8) & 0xFF) as u8;
        desc[36] = ((self.mechanical >> 16) & 0xFF) as u8;
        desc[37] = ((self.mechanical >> 24) & 0xFF) as u8;

        // [38-41] dwFeatures (CRITICAL for libccid behavior)
        desc[38] = (self.features & 0xFF) as u8;
        desc[39] = ((self.features >> 8) & 0xFF) as u8;
        desc[40] = ((self.features >> 16) & 0xFF) as u8;
        desc[41] = ((self.features >> 24) & 0xFF) as u8;

        // [42-45] dwMaxCCIDMessageLength
        desc[42] = (self.max_ccid_message_length & 0xFF) as u8;
        desc[43] = ((self.max_ccid_message_length >> 8) & 0xFF) as u8;
        desc[44] = ((self.max_ccid_message_length >> 16) & 0xFF) as u8;
        desc[45] = ((self.max_ccid_message_length >> 24) & 0xFF) as u8;

        // [46] bClassGetResponse
        desc[46] = self.class_get_response;

        // [47] bClassEnvelope
        desc[47] = self.class_envelope;

        // [48-49] wLcdLayout
        desc[48] = self.lcd_layout.0; // lines
        desc[49] = self.lcd_layout.1; // chars per line

        // [50] bPINSupport
        desc[50] = self.pin_support;

        // [51] bMaxCCIDBusySlots
        desc[51] = self.max_busy_slots;

        desc
    }

    /// Check if this profile uses Short APDU level exchange
    pub const fn is_short_apdu(&self) -> bool {
        matches!(self.exchange_level, ExchangeLevel::ShortApdu)
    }

    /// Check if this profile has PIN pad capability
    pub const fn has_pin_pad(&self) -> bool {
        self.pin_support != 0
    }

    /// Check if this profile has LCD display
    pub const fn has_lcd(&self) -> bool {
        self.lcd_layout.0 > 0 && self.lcd_layout.1 > 0
    }
}

// ============================================================================
// Base Profile (Common Configuration)
// ============================================================================

/// Base profile containing all common CCID configuration values.
///
/// All device profiles share these identical settings:
/// - CCID version 1.1 (bcdCCID = 0x0110)
/// - Single slot (maxSlotIndex = 0)
/// - Voltage support: 5V | 3V | 1.8V
/// - Protocols: T=0 | T=1
/// - Timing: 4-20 MHz clock, up to 344086 bps data rate
/// - Features: Auto config, Short APDU level
/// - Message size: 271 bytes max
/// - Exchange level: Short APDU
///
/// Individual profiles only override USB identity and display/PIN settings.
const BASE_PROFILE: DeviceProfile = DeviceProfile {
    // USB Identity (placeholder - must be overridden)
    vendor_id: 0x0000,
    product_id: 0x0000,
    device_release: 0x0100,
    manufacturer: "",
    product: "",
    serial_number: "",

    // CCID Version
    bcd_ccid: 0x0110,
    max_slot_index: 0,
    voltage_support: 0x07, // 5V | 3V | 1.8V
    protocols: 0x03,       // T=0 | T=1

    // Timing
    default_clock_khz: 4000,
    max_clock_khz: 20000,
    num_clocks: 0,
    default_data_rate: 10752,
    max_data_rate: 344086,
    num_data_rates: 0,

    // T=1 Parameters
    max_ifsd: 254,
    synch_protocols: 0,
    mechanical: 0,

    // Features (CRITICAL):
    // - Auto param config from ATR (bit 1)
    // - Auto ICC clock change (bit 4)
    // - Auto baud rate change (bit 5)
    // - Auto PPS (bit 7)
    // - Short APDU level (bit 17) - NOT TPDU!
    // - NO Auto IFSD (bit 10) - let libccid handle it
    features: FEAT_AUTO_PARAM_ATR
        | FEAT_AUTO_CLOCK
        | FEAT_AUTO_BAUD
        | FEAT_AUTO_PPS
        | FEAT_SHORT_APDU_LEVEL,

    // Message Size
    max_ccid_message_length: 271,

    // Class Bytes
    class_get_response: 0xFF,
    class_envelope: 0xFF,

    // Display/PIN (disabled by default)
    lcd_layout: (0, 0),
    pin_support: 0x00,

    // Concurrency
    max_busy_slots: 1,

    // Exchange level
    exchange_level: ExchangeLevel::ShortApdu,
};

// ============================================================================
// Device Profiles (USB Identity + Display/PIN overrides only)
// ============================================================================

/// Cherry SmartTerminal ST-2100 Profile
///
/// A PIN pad reader with LCD display. We configure it for Short APDU level
/// to match our firmware's APDU-centric `handle_xfr_block` implementation.
///
/// Reference: https://ccid.apdu.fr/ccid/section.html
#[cfg(feature = "profile-cherry-st2100")]
pub const CURRENT_PROFILE: DeviceProfile = DeviceProfile {
    vendor_id: 0x046A,
    product_id: 0x003E,
    manufacturer: "Cherry GmbH",
    product: "SmartTerminal ST-2100",
    serial_number: "ST2100-001",
    ..BASE_PROFILE
};

/// Gemalto IDBridge CT30 Profile (Plain Reader)
///
/// Basic smartcard reader without PIN pad or display.
/// Uses Short APDU level for simplicity.
#[cfg(feature = "profile-gemalto-plain")]
pub const CURRENT_PROFILE: DeviceProfile = DeviceProfile {
    vendor_id: 0x08E6,
    product_id: 0x3437,
    manufacturer: "Gemalto",
    product: "IDBridge CT30",
    serial_number: "CT30-001",
    ..BASE_PROFILE
};

/// Gemalto IDBridge K30 Profile (PIN Pad Reader)
///
/// PIN pad reader with LCD display.
/// Uses Short APDU level with PIN pad enabled.
#[cfg(feature = "profile-gemalto-pinpad")]
pub const CURRENT_PROFILE: DeviceProfile = DeviceProfile {
    vendor_id: 0x08E6,
    product_id: 0x3438,
    manufacturer: "Gemalto",
    product: "IDBridge K30",
    serial_number: "K30-001",
    lcd_layout: (16, 16),
    pin_support: PIN_VERIFY_MODIFY,
    ..BASE_PROFILE
};

/// Compile error if no profile feature is selected.
/// Use `--features profile-cherry-st2100` (default) or another profile.
#[cfg(not(any(
    feature = "profile-cherry-st2100",
    feature = "profile-gemalto-plain",
    feature = "profile-gemalto-pinpad"
)))]
compile_error!(
    "No device profile selected. Use one of: \
     --features profile-cherry-st2100 (default), \
     --features profile-gemalto-plain, \
     --features profile-gemalto-pinpad"
);

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cherry_st2100_descriptor_size() {
        let desc = CURRENT_PROFILE.ccid_descriptor();
        assert_eq!(desc.len(), 52);
    }

    #[test]
    fn test_cherry_st2100_bcd_ccid() {
        let desc = CURRENT_PROFILE.ccid_descriptor();
        // bcdCCID at offset 0-1 = 0x0110 (Rev 1.1)
        assert_eq!(desc[0], 0x10);
        assert_eq!(desc[1], 0x01);
    }

    #[test]
    fn test_cherry_st2100_protocols() {
        let desc = CURRENT_PROFILE.ccid_descriptor();
        let protocols = u32::from_le_bytes([desc[4], desc[5], desc[6], desc[7]]);
        assert_eq!(protocols, 3); // T=0 | T=1
    }

    #[test]
    fn test_cherry_st2100_features_no_auto_ifsd() {
        let desc = CURRENT_PROFILE.ccid_descriptor();
        let features = u32::from_le_bytes([desc[38], desc[39], desc[40], desc[41]]);

        // Must NOT have AUTO_IFSD (bit 10 = 0x0400) to enable XfrBlock
        assert_eq!(features & FEAT_AUTO_IFSD, 0, "AUTO_IFSD must be disabled");

        // Must have Short APDU level (bit 17 = 0x00020000)
        assert_ne!(
            features & FEAT_SHORT_APDU_LEVEL,
            0,
            "Short APDU level required"
        );
    }

    #[test]
    fn test_cherry_st2100_max_ifsd() {
        let desc = CURRENT_PROFILE.ccid_descriptor();
        let max_ifsd = u32::from_le_bytes([desc[26], desc[27], desc[28], desc[29]]);
        assert_eq!(max_ifsd, 254);
    }

    #[test]
    fn test_cherry_st2100_max_message_length() {
        let desc = CURRENT_PROFILE.ccid_descriptor();
        let max_msg = u32::from_le_bytes([desc[42], desc[43], desc[44], desc[45]]);
        assert_eq!(max_msg, 271);
    }

    #[test]
    fn test_exchange_level_short_apdu() {
        assert!(CURRENT_PROFILE.is_short_apdu());
    }

    #[test]
    fn test_pin_pad_disabled() {
        assert!(!CURRENT_PROFILE.has_pin_pad());
    }
}
