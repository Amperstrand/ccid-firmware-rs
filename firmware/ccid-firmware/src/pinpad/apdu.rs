//! Card-answer parsing for the PIN VERIFY command (UI-facing: the pinpad
//! screens show wrong-PIN counters from these SW pairs).
//!
//! The reader-built VERIFY/CHANGE APDU builders moved to the shared core
//! (`ccid_core::secure`, issue #121 track I) together with the
//! PIN-result→APDU exchange; they are re-exported from the module root.

pub use ::ccid_core::secure::{ApduError, ModifyApduBuilder, VerifyApduBuilder};

/// Response APDU parser for VERIFY command
pub struct VerifyResponse {
    /// SW1 byte
    pub sw1: u8,
    /// SW2 byte
    pub sw2: u8,
}

impl VerifyResponse {
    /// Parse response from card
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 2 {
            return None;
        }
        let len = data.len();
        Some(Self {
            sw1: data[len - 2],
            sw2: data[len - 1],
        })
    }

    /// Check if verification was successful
    pub fn is_success(&self) -> bool {
        self.sw1 == 0x90 && self.sw2 == 0x00
    }

    /// Check if PIN was wrong (and get remaining attempts)
    /// Returns Some(remaining_attempts) if PIN was wrong, None otherwise
    pub fn wrong_pin(&self) -> Option<u8> {
        if self.sw1 == 0x63 && (self.sw2 & 0xF0) == 0xC0 {
            Some(self.sw2 & 0x0F)
        } else {
            None
        }
    }

    /// Check if PIN is blocked (no more attempts)
    pub fn is_blocked(&self) -> bool {
        // 0x63C0 means 0 remaining attempts
        self.sw1 == 0x63 && self.sw2 == 0xC0
    }

    /// Check if command was not allowed (e.g., PIN not needed)
    pub fn not_allowed(&self) -> bool {
        self.sw1 == 0x69 && self.sw2 == 0x85
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_response_success() {
        let response = VerifyResponse::parse(&[0x90, 0x00]).unwrap();
        assert!(response.is_success());
        assert!(!response.is_blocked());
        assert!(response.wrong_pin().is_none());
    }

    #[test]
    fn test_verify_response_wrong_pin() {
        let response = VerifyResponse::parse(&[0x63, 0xC2]).unwrap(); // 2 attempts left
        assert!(!response.is_success());
        assert!(!response.is_blocked());
        assert_eq!(response.wrong_pin(), Some(2));
    }

    #[test]
    fn test_verify_response_blocked() {
        let response = VerifyResponse::parse(&[0x63, 0xC0]).unwrap(); // 0 attempts
        assert!(!response.is_success());
        assert!(response.is_blocked());
        assert_eq!(response.wrong_pin(), Some(0));
    }

    #[test]
    fn test_verify_response_not_allowed() {
        let response = VerifyResponse::parse(&[0x69, 0x85]).unwrap();
        assert!(!response.is_success());
        assert!(response.not_allowed());
    }
}
