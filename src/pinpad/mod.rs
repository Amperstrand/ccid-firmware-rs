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
}

impl Default for PinVerifyParams {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            min_len: 6,
            max_len: 8,
            format: 0x82,     // ASCII, left justified
            validation: 0x02, // Validation key pressed
            apdu_template: [0x00, 0x20, 0x00, 0x81, 0x08],
            template_len: 5,
            pin_type: 0x81, // User PIN
            time_slot: 100, // 100ms default
            message_index: 0,
        }
    }
}

impl PinVerifyParams {
    /// Parse PIN verification data structure from CCID message
    ///
    /// CCID PIN Verification Data Structure (Section 6.1.11):
    /// Offset 0: bTimerOut
    /// Offset 1: bmFormatString
    /// Offset 2: bmPINBlockString
    /// Offset 3: bmPINLengthFormat
    /// Offset 4-5: wPINMaxExtraDigit (max:high, min:low)
    /// Offset 6: bEntryValidationCondition
    /// Offset 7: bNumberMessage
    /// Offset 8-9: wLangId
    /// Offset 10: bMsgIndex
    /// Offset 11: bTeoPrologue
    /// Offset 12+: abPINApdu (APDU template)
    pub fn parse(data: &[u8]) -> Option<Self> {
        // Minimum size: header (12 bytes) + APDU template (4+ bytes)
        if data.len() < 16 {
            return None;
        }

        let timeout_secs = data[0];
        let format = data[1];
        let _pin_block_string = data[2];
        let _pin_length_format = data[3];

        // wPINMaxExtraDigit: high byte = max, low byte = min
        let max_len = data[4];
        let min_len = data[5];

        let validation = data[6];
        // Skip bNumberMessage, wLangId, bMsgIndex (bytes 7-10)

        // bTeoPrologue at offset 11
        // abPINApdu starts at offset 12
        let apdu_start = 12;

        // Need at least 4 bytes for APDU header
        if data.len() < apdu_start + 4 {
            return None;
        }

        let cla = data[apdu_start];
        let ins = data[apdu_start + 1];
        let p1 = data[apdu_start + 2];
        let p2 = data[apdu_start + 3];

        // For VERIFY command, we expect INS = 0x20
        // The template includes Lc, but we'll compute it based on actual PIN length
        let apdu_template = [cla, ins, p1, p2, max_len];

        Some(Self {
            timeout_secs,
            min_len,
            max_len,
            format,
            validation,
            apdu_template,
            template_len: 5,
            pin_type: p2,   // 0x81 = User PIN (PW1), 0x83 = Admin PIN (PW3)
            time_slot: 100, // Default 100ms
            message_index: data.get(10).copied().unwrap_or(0),
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

    /// Get the PIN as ASCII bytes
    pub fn as_ascii(&self) -> &[u8] {
        // Convert digits to ASCII: '0' = 0x30, '1' = 0x31, etc.
        // This is done lazily via conversion
        &self.digits[..self.len]
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

    /// Get raw digit at index
    pub fn get(&self, index: usize) -> Option<u8> {
        if index < self.len {
            Some(self.digits[index])
        } else {
            None
        }
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

/// Securely clear a byte slice
pub fn secure_clear(data: &mut [u8]) {
    for b in data.iter_mut() {
        unsafe {
            core::ptr::write_volatile(b, 0);
        }
    }
}
