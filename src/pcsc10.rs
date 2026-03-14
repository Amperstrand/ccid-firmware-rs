//! PC/SC Part 10 Feature Discovery and TLV Properties Implementation
//!
//! This module implements the PC/SC Part 10 v2.02.09 feature discovery mechanism
//! used by host drivers (pcsc-lite, Windows Smart Card subsystem) to discover
//! reader capabilities including secure PIN entry features.
//!
//! # Architecture
//!
//! Host software discovers features via:
//! 1. `CM_IOCTL_GET_FEATURE_REQUEST` (SCardControl) → Returns feature TLV list
//! 2. Each feature has a tag (1 byte) and control code (4 bytes, big-endian)
//! 3. For TLV properties, `FEATURE_GET_TLV_PROPERTIES` returns detailed capabilities
//!
//! # References
//!
//! - PC/SC Part 10 v2.02.09 Sections 4.2-4.4
//! - CCID Rev 1.1 Spec Sections 6.1.11-6.1.12
//! - pcsc-lite reader.h for feature tag definitions
//!   https://github.com/LudovicRousseau/PCSC/blob/master/src/PCSC/reader.h

#![allow(dead_code)]
#![allow(non_snake_case)]

// ============================================================================
// PC/SC Part 10 Feature Tags
// ============================================================================
// Reference: PC/SC Part 10 v2.02.09 Section 4.2
// Reference: pcsc-lite src/PCSC/reader.h lines 130-142

/// Feature tag for VERIFY_PIN_DIRECT (0x06)
pub const FEATURE_VERIFY_PIN_DIRECT: u8 = 0x06;

/// Feature tag for MODIFY_PIN_DIRECT (0x07)
pub const FEATURE_MODIFY_PIN_DIRECT: u8 = 0x07;

/// Feature tag for IFD_PIN_PROPERTIES (0x0A)
pub const FEATURE_IFD_PIN_PROPERTIES: u8 = 0x0A;

/// Feature tag for GET_TLV_PROPERTIES (0x12)
pub const FEATURE_GET_TLV_PROPERTIES: u8 = 0x12;

/// Feature tag for CCID_ESC_COMMAND (0x13)
pub const FEATURE_CCID_ESC_COMMAND: u8 = 0x13;

// ============================================================================
// TLV Properties Tags
// ============================================================================
// Reference: PC/SC Part 10 v2.02.09 Section 4.4
// Reference: pcsc-lite src/PCSC/reader.h lines 257-266

/// TLV tag for wLcdLayout (0x01)
pub const TLV_WLCDLAYOUT: u8 = 0x01;

/// TLV tag for bEntryValidationCondition (0x02)
pub const TLV_BENTRYVALIDATIONCONDITION: u8 = 0x02;

/// TLV tag for bTimeOut2 (0x03)
pub const TLV_BTIMEOUT2: u8 = 0x03;

/// TLV tag for wLcdMaxCharacters (0x04)
pub const TLV_WLCDMAXCHARACTERS: u8 = 0x04;

/// TLV tag for wLcdMaxLines (0x05)
pub const TLV_WLCDMAXLINES: u8 = 0x05;

/// TLV tag for bMinPINSize (0x06)
pub const TLV_BMINPINSIZE: u8 = 0x06;

/// TLV tag for bMaxPINSize (0x07)
pub const TLV_BMAXPINSIZE: u8 = 0x07;

/// TLV tag for sFirmwareID (0x08)
pub const TLV_SFIRMWAREID: u8 = 0x08;

/// TLV tag for bPPDUSupport (0x09)
pub const TLV_BPPDUSUPPORT: u8 = 0x09;

/// TLV tag for dwMaxAPDUDataSize (0x0A)
pub const TLV_DWMAXAPDUDATASIZE: u8 = 0x0A;

/// TLV tag for wIdVendor (0x0B)
pub const TLV_WIDVENDOR: u8 = 0x0B;

/// TLV tag for wIdProduct (0x0C)
pub const TLV_WIDPRODUCT: u8 = 0x0C;

// ============================================================================
// PIN Entry Validation Conditions
// ============================================================================
// Reference: CCID Rev 1.1 Spec Section 6.1.11

/// Validation by pressing OK/Confirm key (bit 1)
pub const VALIDATION_KEY: u8 = 0x02;

/// Validation after timeout (bit 0)
pub const VALIDATION_TIMEOUT: u8 = 0x01;

/// Validation after max PIN length reached (bit 2)
pub const VALIDATION_MAX_LENGTH: u8 = 0x04;

// ============================================================================
// Feature Discovery Response Builder
// ============================================================================
//
// GET_FEATURE_REQUEST response format (per PC/SC Part 10 Section 2.2):
// - Concatenation of TLV entries
// - Tag: 1 byte (feature identifier)
// - Length: 1 byte (always 4 for control codes)
// - Value: 4 bytes (control code, BIG-ENDIAN)
//
// IMPORTANT: Control codes are BIG-ENDIAN per reader.h line 164:
// "This value is always in BIG ENDIAN format as documented in PCSC v2 part 10 ch 2.2 page 2."

/// Builder for GET_FEATURE_REQUEST response
pub struct FeatureDiscovery {
    buffer: [u8; 64],
    len: usize,
}

impl FeatureDiscovery {
    /// Create a new feature discovery builder
    pub const fn new() -> Self {
        Self {
            buffer: [0u8; 64],
            len: 0,
        }
    }

    /// Add a feature TLV entry
    ///
    /// # Arguments
    /// * `tag` - Feature tag (e.g., FEATURE_VERIFY_PIN_DIRECT)
    /// * `control_code` - Control code for SCardControl (BIG-ENDIAN per PC/SC spec)
    pub const fn add_feature(mut self, tag: u8, control_code: u32) -> Self {
        if self.len + 6 > self.buffer.len() {
            return self;
        }

        // TLV format for GET_FEATURE_REQUEST:
        // Tag (1 byte) | Length (1 byte, always 4) | Value (4 bytes, big-endian)
        self.buffer[self.len] = tag;
        self.buffer[self.len + 1] = 4; // Length is always 4 for control codes
                                       // Control code is BIG-ENDIAN for GET_FEATURE_REQUEST
        self.buffer[self.len + 2] = ((control_code >> 24) & 0xFF) as u8;
        self.buffer[self.len + 3] = ((control_code >> 16) & 0xFF) as u8;
        self.buffer[self.len + 4] = ((control_code >> 8) & 0xFF) as u8;
        self.buffer[self.len + 5] = (control_code & 0xFF) as u8;
        self.len += 6;

        self
    }

    /// Build the feature discovery response for a device with PIN pad
    pub const fn build_for_pinpad() -> Self {
        let mut discovery = Self::new();

        // Add PIN verification
        // Control code format: CM_IOCTL_GET_FEATURE_REQUEST returns platform-specific codes
        // On Linux these are typically SCARD_CTL_CODE(n) = 0x42000000 + (n << 2)
        discovery = discovery.add_feature(FEATURE_VERIFY_PIN_DIRECT, 0x42000D48);

        // Add PIN modification
        discovery = discovery.add_feature(FEATURE_MODIFY_PIN_DIRECT, 0x42000D49);

        // Add IFD_PIN_PROPERTIES for retrieving PIN properties
        discovery = discovery.add_feature(FEATURE_IFD_PIN_PROPERTIES, 0x42000D4A);

        // Add GET_TLV_PROPERTIES support
        discovery = discovery.add_feature(FEATURE_GET_TLV_PROPERTIES, 0x42000D4B);

        discovery
    }

    /// Get the response bytes
    pub const fn as_bytes(&self) -> &[u8] {
        // SAFETY: We only expose the used portion
        unsafe { core::slice::from_raw_parts(self.buffer.as_ptr(), self.len) }
    }

    /// Get the response as a fixed array (for compile-time use)
    pub const fn into_array<const N: usize>(self) -> ([u8; N], usize) {
        let mut arr = [0u8; N];
        let copy_len = if N < self.len { N } else { self.len };
        let mut i = 0;
        while i < copy_len {
            arr[i] = self.buffer[i];
            i += 1;
        }
        (arr, self.len)
    }

    /// Get the length of the response
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Check if the response is empty
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get the raw buffer (for runtime access)
    pub fn buffer(&self) -> &[u8] {
        &self.buffer[..self.len]
    }
}

impl Default for FeatureDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// TLV Properties Response Builder
// ============================================================================
//
// GET_TLV_PROPERTIES response format (per PC/SC Part 10 Section 4.4):
// - Concatenation of TLV entries
// - Tag: 1 byte
// - Length: 1 byte
// - Value: variable length, LITTLE-ENDIAN for multi-byte values
//
// IMPORTANT: Unlike GET_FEATURE_REQUEST, TLV properties use LITTLE-ENDIAN
// for multi-byte values per CCID/USB conventions.

/// Builder for GET_TLV_PROPERTIES response
pub struct TlvProperties {
    buffer: [u8; 128],
    len: usize,
}

impl TlvProperties {
    /// Create a new TLV properties builder
    pub const fn new() -> Self {
        Self {
            buffer: [0u8; 128],
            len: 0,
        }
    }

    /// Add a byte property (tag + 1 byte value)
    pub const fn add_u8(mut self, tag: u8, value: u8) -> Self {
        if self.len + 2 > self.buffer.len() {
            return self;
        }
        self.buffer[self.len] = tag;
        self.buffer[self.len + 1] = value;
        self.len += 2;
        self
    }

    /// Add a 16-bit property (tag + 2 bytes, little-endian)
    pub const fn add_u16(mut self, tag: u8, value: u16) -> Self {
        if self.len + 3 > self.buffer.len() {
            return self;
        }
        self.buffer[self.len] = tag;
        // Little-endian encoding per CCID/USB conventions
        self.buffer[self.len + 1] = (value & 0xFF) as u8;
        self.buffer[self.len + 2] = ((value >> 8) & 0xFF) as u8;
        self.len += 3;
        self
    }

    /// Add a 32-bit property (tag + 4 bytes, little-endian)
    pub const fn add_u32(mut self, tag: u8, value: u32) -> Self {
        if self.len + 5 > self.buffer.len() {
            return self;
        }
        self.buffer[self.len] = tag;
        // Little-endian encoding per CCID/USB conventions
        self.buffer[self.len + 1] = (value & 0xFF) as u8;
        self.buffer[self.len + 2] = ((value >> 8) & 0xFF) as u8;
        self.buffer[self.len + 3] = ((value >> 16) & 0xFF) as u8;
        self.buffer[self.len + 4] = ((value >> 24) & 0xFF) as u8;
        self.len += 5;
        self
    }

    /// Add a string property (tag + length + bytes)
    pub const fn add_str(mut self, tag: u8, value: &[u8]) -> Self {
        let total_len = 2 + value.len();
        if self.len + total_len > self.buffer.len() {
            return self;
        }
        self.buffer[self.len] = tag;
        self.buffer[self.len + 1] = value.len() as u8;
        let mut i = 0;
        while i < value.len() {
            self.buffer[self.len + 2 + i] = value[i];
            i += 1;
        }
        self.len += total_len;
        self
    }

    /// Build TLV properties for a typical pinpad reader
    pub const fn build_for_pinpad(
        lcd_lines: u8,
        lcd_chars: u8,
        min_pin_size: u8,
        max_pin_size: u8,
        vendor_id: u16,
        product_id: u16,
    ) -> Self {
        let mut props = Self::new();

        // wLcdLayout (0x01): lines << 8 | chars_per_line
        if lcd_lines > 0 && lcd_chars > 0 {
            let lcd_layout = ((lcd_lines as u16) << 8) | (lcd_chars as u16);
            props = props.add_u16(TLV_WLCDLAYOUT, lcd_layout);

            // Additional LCD properties
            props = props.add_u16(TLV_WLCDMAXLINES, lcd_lines as u16);
            props = props.add_u16(TLV_WLCDMAXCHARACTERS, lcd_chars as u16);
        } else {
            props = props.add_u16(TLV_WLCDLAYOUT, 0x0000);
        }

        // bEntryValidationCondition: what triggers PIN validation
        // We support OK key press (0x02) and max length reached (0x04)
        let validation = VALIDATION_KEY | VALIDATION_MAX_LENGTH;
        props = props.add_u8(TLV_BENTRYVALIDATIONCONDITION, validation);

        // bTimeOut2: timeout for PIN entry after first key press
        // 0 means use bTimeOut from PIN structure
        props = props.add_u8(TLV_BTIMEOUT2, 0);

        // PIN size limits
        props = props.add_u8(TLV_BMINPINSIZE, min_pin_size);
        props = props.add_u8(TLV_BMAXPINSIZE, max_pin_size);

        // Firmware ID (optional)
        props = props.add_str(TLV_SFIRMWAREID, b"ccid-firmware-rs 0.0.8");

        // bPPDUSupport: secure PIN over PPDU support
        // 0x01 = short APDU only
        props = props.add_u8(TLV_BPPDUSUPPORT, 0x01);

        // dwMaxAPDUDataSize: maximum APDU data size
        // 261 bytes for short APDU (255 data + 5 header + 1 status)
        props = props.add_u32(TLV_DWMAXAPDUDATASIZE, 261);

        // Vendor/Product ID
        props = props.add_u16(TLV_WIDVENDOR, vendor_id);
        props = props.add_u16(TLV_WIDPRODUCT, product_id);

        props
    }

    /// Get the raw buffer (for runtime access)
    pub fn buffer(&self) -> &[u8] {
        &self.buffer[..self.len]
    }

    /// Get the length of the response
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Check if the response is empty
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for TlvProperties {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// PIN Properties Structure
// ============================================================================
//
// Reference: PC/SC Part 10 v2.02.09 Section 4.1.8
// Reference: pcsc-lite src/PCSC/reader.h lines 243-247

/// PIN Properties structure returned by FEATURE_IFD_PIN_PROPERTIES
///
/// Response format:
/// - wLcdLayout: 2 bytes (High byte=rows, Low byte=columns)
/// - bEntryValidationCondition: 1 byte (bitmask)
/// - bTimeOut2: 1 byte (timeout in seconds)
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PinProperties {
    /// LCD layout: lines in high byte, characters per line in low byte
    pub wLcdLayout: u16,
    /// Entry validation conditions
    pub bEntryValidationCondition: u8,
    /// Timeout after first key press (0 = use bTimeOut)
    pub bTimeOut2: u8,
}

impl PinProperties {
    /// Create PIN properties for a pinpad reader
    pub const fn new(lcd_lines: u8, lcd_chars: u8) -> Self {
        Self {
            wLcdLayout: if lcd_lines > 0 && lcd_chars > 0 {
                ((lcd_lines as u16) << 8) | (lcd_chars as u16)
            } else {
                0x0000
            },
            bEntryValidationCondition: VALIDATION_KEY | VALIDATION_MAX_LENGTH,
            bTimeOut2: 0,
        }
    }

    /// Convert to bytes (little-endian)
    pub const fn to_bytes(&self) -> [u8; 4] {
        [
            (self.wLcdLayout & 0xFF) as u8,
            ((self.wLcdLayout >> 8) & 0xFF) as u8,
            self.bEntryValidationCondition,
            self.bTimeOut2,
        ]
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_discovery_basic() {
        let discovery = FeatureDiscovery::new().add_feature(FEATURE_VERIFY_PIN_DIRECT, 0x42000D48);

        let bytes = discovery.buffer();
        assert_eq!(bytes.len(), 6);
        assert_eq!(bytes[0], FEATURE_VERIFY_PIN_DIRECT);
        assert_eq!(bytes[1], 4); // Length
                                 // Big-endian control code
        assert_eq!(bytes[2], 0x42);
        assert_eq!(bytes[3], 0x00);
        assert_eq!(bytes[4], 0x0D);
        assert_eq!(bytes[5], 0x48);
    }

    #[test]
    fn test_feature_discovery_for_pinpad() {
        let discovery = FeatureDiscovery::build_for_pinpad();

        // Should have 4 features (6 bytes each)
        assert_eq!(discovery.len(), 24);

        let bytes = discovery.buffer();

        // First feature should be VERIFY_PIN_DIRECT
        assert_eq!(bytes[0], FEATURE_VERIFY_PIN_DIRECT);

        // Second feature should be MODIFY_PIN_DIRECT
        assert_eq!(bytes[6], FEATURE_MODIFY_PIN_DIRECT);

        // Third feature should be IFD_PIN_PROPERTIES
        assert_eq!(bytes[12], FEATURE_IFD_PIN_PROPERTIES);

        // Fourth feature should be GET_TLV_PROPERTIES
        assert_eq!(bytes[18], FEATURE_GET_TLV_PROPERTIES);
    }

    #[test]
    fn test_tlv_properties_basic() {
        let props = TlvProperties::new()
            .add_u8(TLV_BMINPINSIZE, 4)
            .add_u8(TLV_BMAXPINSIZE, 12);

        let bytes = props.buffer();
        assert_eq!(bytes.len(), 4);
        assert_eq!(bytes[0], TLV_BMINPINSIZE);
        assert_eq!(bytes[1], 4);
        assert_eq!(bytes[2], TLV_BMAXPINSIZE);
        assert_eq!(bytes[3], 12);
    }

    #[test]
    fn test_tlv_properties_u16() {
        let props = TlvProperties::new().add_u16(TLV_WLCDLAYOUT, 0x0414);

        let bytes = props.buffer();
        assert_eq!(bytes.len(), 3);
        assert_eq!(bytes[0], TLV_WLCDLAYOUT);
        // Little-endian
        assert_eq!(bytes[1], 0x14);
        assert_eq!(bytes[2], 0x04);
    }

    #[test]
    fn test_tlv_properties_for_pinpad() {
        let props = TlvProperties::build_for_pinpad(
            4,      // lcd_lines
            20,     // lcd_chars
            4,      // min_pin_size
            12,     // max_pin_size
            0x046A, // vendor_id
            0x003E, // product_id
        );

        let bytes = props.buffer();

        // Should have multiple properties
        assert!(!props.is_empty());

        // First property should be wLcdLayout
        assert_eq!(bytes[0], TLV_WLCDLAYOUT);
        // wLcdLayout = 0x0414 (4 lines, 20 chars)
        assert_eq!(bytes[1], 0x14); // Low byte (chars)
        assert_eq!(bytes[2], 0x04); // High byte (lines)
    }

    #[test]
    fn test_pin_properties() {
        let pin_props = PinProperties::new(4, 20);
        let bytes = pin_props.to_bytes();

        assert_eq!(bytes.len(), 4);

        // wLcdLayout = 0x0414
        assert_eq!(bytes[0], 0x14); // Low byte (chars)
        assert_eq!(bytes[1], 0x04); // High byte (lines)

        // Validation condition should include key press and max length
        assert_ne!(bytes[2] & VALIDATION_KEY, 0);
        assert_ne!(bytes[2] & VALIDATION_MAX_LENGTH, 0);
    }

    #[test]
    fn test_endian_consistency() {
        // GET_FEATURE_REQUEST: big-endian control codes
        let discovery = FeatureDiscovery::new().add_feature(0x06, 0x12345678);
        let bytes = discovery.buffer();
        // Big-endian: 0x12 0x34 0x56 0x78
        assert_eq!(bytes[2], 0x12);
        assert_eq!(bytes[3], 0x34);
        assert_eq!(bytes[4], 0x56);
        assert_eq!(bytes[5], 0x78);

        // GET_TLV_PROPERTIES: little-endian values
        let props = TlvProperties::new().add_u32(TLV_DWMAXAPDUDATASIZE, 0x12345678);
        let bytes = props.buffer();
        // Little-endian: 0x78 0x56 0x34 0x12
        assert_eq!(bytes[1], 0x78);
        assert_eq!(bytes[2], 0x56);
        assert_eq!(bytes[3], 0x34);
        assert_eq!(bytes[4], 0x12);
    }
}
