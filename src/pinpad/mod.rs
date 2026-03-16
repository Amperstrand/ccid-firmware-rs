//! Pinpad module for secure PIN entry
//!
//! This module provides secure PIN entry functionality for CCID smartcard readers.
//! It includes:
//! - APDU construction for VERIFY commands
//! - PIN verification data structure parsing
//! - State machine for PIN entry flow
//! - Touchscreen-based pinpad UI (embedded-graphics)
//!
//! # Architecture
#![allow(dead_code)]
#![allow(unused_imports)]
//!
//! The module is designed to work on both embedded ARM targets and host machines
//! for testing. All modules use embedded-graphics which works on both platforms.

pub mod apdu;
pub mod state;
#[cfg(feature = "display")]
pub mod ui;

// Re-export common types
pub use apdu::{ApduError, VerifyApduBuilder, VerifyResponse};
pub use state::{PinEntryContext, PinEntryState};
#[cfg(feature = "display")]
pub use ui::{draw_pinpad, ButtonId, Keypad, TouchHandler};

/// PIN operation result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(all(target_arch = "arm", target_os = "none"), derive(defmt::Format))]
pub enum PinResult {
    /// PIN verified successfully
    Success,
    /// User cancelled PIN entry
    Cancelled,
    /// PIN entry timed out
    Timeout,
    /// Invalid PIN length
    InvalidLength,
}

/// PIN modification parameters extracted from CCID message (CCID Rev 1.1 §6.1.12)
///
/// | Offset | Field | Description |
/// |--------|-------|-------------|
/// | 0 | bTimerOut | Timeout in seconds (0 = no timeout) |
/// | 1 | bmFormatString | PIN format and justification |
/// | 2 | bmPINBlockString | PIN block length |
/// | 3 | bmPINLengthFormat | PIN length format |
/// | 4 | bInsertionOffsetOld | Offset for old PIN in APDU |
/// | 5 | bInsertionOffsetNew | Offset for new PIN in APDU |
/// | 6-7 | wPINMaxExtraDigit | Max (high), Min (low) |
/// | 8 | bEntryValidationCondition | Validation trigger |
/// | 9 | bNumberMessage | Number of messages |
/// | 10-11 | wLangId | Language ID |
/// | 12 | bMsgIndex1 | Message index for old PIN prompt |
/// | 13 | bMsgIndex2 | Message index for new PIN prompt |
/// | 14 | bMsgIndex3 | Message index for confirm PIN prompt |
/// | 15 | bTeoPrologue | TPDU prologue |
/// | 16+ | abPINApdu | APDU template |
#[derive(Debug, Clone, Copy)]
pub struct PinModifyParams {
    /// Timeout in seconds (0 = no timeout)
    pub timeout_secs: u8,
    /// Minimum PIN length
    pub min_len: u8,
    /// Maximum PIN length
    pub max_len: u8,
    /// PIN format flags (bmFormatString)
    pub format: u8,
    /// Entry validation condition
    pub validation: u8,
    /// Offset in APDU where old PIN should be inserted
    pub old_pin_offset: u8,
    /// Offset in APDU where new PIN should be inserted
    pub new_pin_offset: u8,
    /// APDU template (CLA INS P1 P2 [Lc])
    pub apdu_template: [u8; 5],
    /// Template length (typically 5 for short APDU)
    pub template_len: usize,
    /// P2 value to determine PIN type (0x81=user, 0x83=admin)
    pub pin_type: u8,
    /// Time slot for UI updates (milliseconds)
    pub time_slot: u16,
    /// Message index for old PIN prompt
    pub msg_index_old: u8,
    /// Message index for new PIN prompt
    pub msg_index_new: u8,
    /// Message index for confirm PIN prompt
    pub msg_index_confirm: u8,
}

impl Default for PinModifyParams {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            min_len: 6,
            max_len: 8,
            format: 0x82,                                  // ASCII, left justified
            validation: 0x02,                              // Validation key pressed
            old_pin_offset: 5,                             // After CLA INS P1 P2 Lc
            new_pin_offset: 13,                            // After old PIN (8 bytes)
            apdu_template: [0x00, 0x24, 0x00, 0x81, 0x10], // CHANGE REFERENCE
            template_len: 5,
            pin_type: 0x81, // User PIN
            time_slot: 100,
            msg_index_old: 0,
            msg_index_new: 1,
            msg_index_confirm: 2,
        }
    }
}

impl PinModifyParams {
    /// Parse PIN modification data structure from CCID Rev 1.1 §6.1.12
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 20 {
            return None;
        }

        let timeout_secs = data[0];
        let format = data[1];
        let _pin_block_string = data[2];
        let _pin_length_format = data[3];
        let old_pin_offset = data[4];
        let new_pin_offset = data[5];

        let max_len = data[6];
        let min_len = data[7];

        let validation = data[8];

        let apdu_start = 16; // Offset 16 per CCID spec

        if data.len() < apdu_start + 4 {
            return None;
        }

        let cla = data[apdu_start];
        let ins = data[apdu_start + 1];
        let p1 = data[apdu_start + 2];
        let p2 = data[apdu_start + 3];

        let apdu_template = [cla, ins, p1, p2, max_len * 2]; // Lc = old + new

        Some(Self {
            timeout_secs,
            min_len,
            max_len,
            format,
            validation,
            old_pin_offset,
            new_pin_offset,
            apdu_template,
            template_len: 5,
            pin_type: p2,
            time_slot: 100,
            msg_index_old: data.get(12).copied().unwrap_or(0),
            msg_index_new: data.get(13).copied().unwrap_or(1),
            msg_index_confirm: data.get(14).copied().unwrap_or(2),
        })
    }

    /// Check if this is a User PIN modification
    pub fn is_user_pin(&self) -> bool {
        self.pin_type == 0x81
    }

    /// Check if this is an Admin PIN modification
    pub fn is_admin_pin(&self) -> bool {
        self.pin_type == 0x83
    }
}

/// PIN verification parameters extracted from CCID message
#[derive(Debug, Clone, Copy)]
pub struct PinVerifyParams {
    /// Timeout in seconds (0 = no timeout)
    pub timeout_secs: u8,
    /// Minimum PIN length
    pub min_len: u8,
    /// Maximum PIN length
    pub max_len: u8,
    /// PIN format flags (bmFormatString)
    pub format: u8,
    /// Entry validation condition
    pub validation: u8,
    /// APDU template (CLA INS P1 P2 [Lc])
    pub apdu_template: [u8; 5],
    /// Template length (typically 5 for short APDU)
    pub template_len: usize,
    /// P2 value to determine PIN type (0x81=user, 0x83=admin)
    pub pin_type: u8,
    /// Time slot for UI updates (milliseconds)
    pub time_slot: u16,
    /// Message index for display
    pub message_index: u8,
    /// PIN block string (CCID offset 2) - PIN block size in bytes
    pub pin_block_string: u8,
    /// PIN length format (CCID offset 3) - how PIN length is encoded
    pub pin_length_format: u8,
    /// Number of messages to display (CCID offset 7)
    pub number_message: u8,
    /// Language ID (CCID offsets 8-9)
    pub lang_id: u16,
    /// TPDU prologue byte (CCID offset 11)
    pub teo_prologue: u8,
}

impl Default for PinVerifyParams {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            min_len: 6,
            max_len: 8,
            format: 0x82,
            validation: 0x02,
            apdu_template: [0x00, 0x20, 0x00, 0x81, 0x08],
            template_len: 5,
            pin_type: 0x81,
            time_slot: 100,
            message_index: 0,
            pin_block_string: 0x08,
            pin_length_format: 0x00,
            number_message: 0x01,
            lang_id: 0x0409,
            teo_prologue: 0x00,
        }
    }
}

impl PinVerifyParams {
    /// Parse PIN verification data structure from CCID Rev 1.1 §6.1.11
    ///
    /// | Offset | Field | Description |
    /// |--------|-------|-------------|
    /// | 0 | bTimerOut | Timeout in seconds (0 = no timeout) |
    /// | 1 | bmFormatString | PIN format and justification |
    /// | 2 | bmPINBlockString | PIN block length |
    /// | 3 | bmPINLengthFormat | PIN length format |
    /// | 4-5 | wPINMaxExtraDigit | Max (high), Min (low) |
    /// | 6 | bEntryValidationCondition | Validation trigger |
    /// | 7 | bNumberMessage | Number of messages |
    /// | 8-9 | wLangId | Language ID |
    /// | 10 | bMsgIndex | Message index |
    /// | 11 | bTeoPrologue | TPDU prologue |
    /// | 12+ | abPINApdu | APDU template |
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 16 {
            return None;
        }

        let timeout_secs = data[0];
        let format = data[1];
        let pin_block_string = data[2];
        let pin_length_format = data[3];

        let max_len = data[4];
        let min_len = data[5];

        let validation = data[6];
        let number_message = data.get(7).copied().unwrap_or(1);
        let lang_id = u16::from_le_bytes([
            data.get(8).copied().unwrap_or(0x09),
            data.get(9).copied().unwrap_or(0x04),
        ]);
        let teo_prologue = data.get(11).copied().unwrap_or(0);

        let apdu_start = 12;

        if data.len() < apdu_start + 4 {
            return None;
        }

        let cla = data[apdu_start];
        let ins = data[apdu_start + 1];
        let p1 = data[apdu_start + 2];
        let p2 = data[apdu_start + 3];

        let apdu_template = [cla, ins, p1, p2, max_len];

        Some(Self {
            timeout_secs,
            min_len,
            max_len,
            format,
            validation,
            apdu_template,
            template_len: 5,
            pin_type: p2,
            time_slot: 100,
            message_index: data.get(10).copied().unwrap_or(0),
            pin_block_string,
            pin_length_format,
            number_message,
            lang_id,
            teo_prologue,
        })
    }

    /// Check if this is a User PIN verification
    pub fn is_user_pin(&self) -> bool {
        self.pin_type == 0x81
    }

    /// Check if this is an Admin PIN verification
    pub fn is_admin_pin(&self) -> bool {
        self.pin_type == 0x83
    }
}

/// PIN buffer for storing entered digits
#[derive(Debug, Clone)]
pub struct PinBuffer {
    digits: [u8; 16],
    len: usize,
    max_len: usize,
}

impl PinBuffer {
    /// Create a new PIN buffer with maximum length
    pub fn new(max_len: usize) -> Self {
        Self {
            digits: [0; 16],
            len: 0,
            max_len: max_len.min(16),
        }
    }

    /// Add a digit to the buffer
    pub fn push(&mut self, digit: u8) -> bool {
        if self.len >= self.max_len {
            return false;
        }
        if digit > 9 {
            return false;
        }
        self.digits[self.len] = digit;
        self.len += 1;
        true
    }

    /// Remove the last digit
    pub fn pop(&mut self) -> bool {
        if self.len == 0 {
            return false;
        }
        self.len -= 1;
        true
    }

    /// Clear the buffer
    pub fn clear(&mut self) {
        for d in self.digits.iter_mut() {
            unsafe {
                core::ptr::write_volatile(d, 0);
            }
        }
        self.len = 0;
    }

    /// Get current length
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Check if buffer has minimum required digits
    pub fn has_minimum(&self, min: usize) -> bool {
        self.len >= min
    }

    /// Convert PIN to ASCII representation
    pub fn to_ascii(&self) -> [u8; 16] {
        let mut ascii = [0u8; 16];
        for (i, &d) in self.digits[..self.len].iter().enumerate() {
            ascii[i] = d + b'0';
        }
        ascii
    }

    /// Get masked representation for display (e.g., "****")
    pub fn to_mask(&self) -> [u8; 16] {
        let mut mask = [0u8; 16];
        for item in mask.iter_mut().take(self.len) {
            *item = b'*';
        }
        mask
    }
}

impl Drop for PinBuffer {
    fn drop(&mut self) {
        // Securely clear the buffer
        for d in self.digits.iter_mut() {
            unsafe {
                core::ptr::write_volatile(d, 0);
            }
        }
    }
}
