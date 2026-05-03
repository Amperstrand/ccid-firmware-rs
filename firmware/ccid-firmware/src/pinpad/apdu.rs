//! APDU construction for PIN verification and modification

#![allow(dead_code)]

pub struct VerifyApduBuilder {
    cla: u8,
    p1: u8,
    p2: u8,
}

pub struct ModifyApduBuilder {
    cla: u8,
    p1: u8,
    p2: u8,
    old_pin_offset: usize,
    new_pin_offset: usize,
}

impl VerifyApduBuilder {
    /// Create a new builder for User PIN verification
    pub fn user_pin() -> Self {
        Self {
            cla: 0x00,
            p1: 0x00,
            p2: 0x81,
        }
    }

    /// Create a new builder for Admin PIN verification
    pub fn admin_pin() -> Self {
        Self {
            cla: 0x00,
            p1: 0x00,
            p2: 0x83,
        }
    }

    /// Create a builder from raw APDU template
    pub fn from_template(cla: u8, p1: u8, p2: u8) -> Self {
        Self { cla, p1, p2 }
    }

    /// Check if this is for User PIN
    pub fn is_user_pin(&self) -> bool {
        self.p2 == 0x81
    }

    /// Check if this is for Admin PIN
    pub fn is_admin_pin(&self) -> bool {
        self.p2 == 0x83
    }

    /// Build the VERIFY APDU from ASCII PIN digits
    ///
    /// Input: ASCII digits ('0'-'9')
    /// Output: Complete APDU ready to send to card
    ///
    /// Format:
    /// - CLA INS P1 P2 Lc <PIN>
    /// - Where PIN is the ASCII representation
    pub fn build(&self, pin_ascii: &[u8]) -> Result<[u8; 13], ApduError> {
        // Validate PIN length
        if pin_ascii.is_empty() {
            return Err(ApduError::PinTooShort);
        }

        // Check for Admin PIN - must be exactly 8 characters
        if self.is_admin_pin() && pin_ascii.len() != 8 {
            return Err(ApduError::InvalidPinLength);
        }

        // Check for User PIN - must be 6-8 characters
        if self.is_user_pin() && (pin_ascii.len() < 6 || pin_ascii.len() > 8) {
            return Err(ApduError::InvalidPinLength);
        }

        // Validate all characters are ASCII digits
        for &c in pin_ascii {
            if !c.is_ascii_digit() {
                return Err(ApduError::InvalidPinCharacter);
            }
        }

        // Build APDU
        let mut apdu = [0u8; 13];
        let pin_len = pin_ascii.len();

        apdu[0] = self.cla;
        apdu[1] = 0x20; // INS = VERIFY
        apdu[2] = self.p1;
        apdu[3] = self.p2;
        apdu[4] = pin_len as u8; // Lc

        // Copy PIN data
        apdu[5..5 + pin_len].copy_from_slice(pin_ascii);

        // Clear rest of buffer
        for b in apdu[5 + pin_len..].iter_mut() {
            *b = 0;
        }

        Ok(apdu)
    }

    /// Build the VERIFY APDU from raw digit values
    ///
    /// Input: Raw digits (0-9)
    /// Output: Complete APDU with ASCII PIN
    pub fn build_from_digits(&self, digits: &[u8]) -> Result<[u8; 13], ApduError> {
        // Convert digits to ASCII
        let mut ascii = [0u8; 8];
        let len = digits.len().min(8);

        for (i, &d) in digits[..len].iter().enumerate() {
            if d > 9 {
                return Err(ApduError::InvalidPinCharacter);
            }
            ascii[i] = d + b'0';
        }

        self.build(&ascii[..len])
    }

    pub fn apdu_len(pin_len: usize) -> usize {
        5 + pin_len
    }
}

impl ModifyApduBuilder {
    pub fn user_pin() -> Self {
        Self {
            cla: 0x00,
            p1: 0x00,
            p2: 0x81,
            old_pin_offset: 5,
            new_pin_offset: 13,
        }
    }

    pub fn admin_pin() -> Self {
        Self {
            cla: 0x00,
            p1: 0x00,
            p2: 0x83,
            old_pin_offset: 5,
            new_pin_offset: 13,
        }
    }

    pub fn from_template(cla: u8, p1: u8, p2: u8, old_off: usize, new_off: usize) -> Self {
        Self {
            cla,
            p1,
            p2,
            old_pin_offset: old_off,
            new_pin_offset: new_off,
        }
    }

    pub fn is_user_pin(&self) -> bool {
        self.p2 == 0x81
    }

    pub fn is_admin_pin(&self) -> bool {
        self.p2 == 0x83
    }

    pub fn build(&self, old_pin: &[u8], new_pin: &[u8]) -> Result<[u8; 21], ApduError> {
        if old_pin.is_empty() || new_pin.is_empty() {
            return Err(ApduError::PinTooShort);
        }

        if self.is_admin_pin() && (old_pin.len() != 8 || new_pin.len() != 8) {
            return Err(ApduError::InvalidPinLength);
        }

        if self.is_user_pin()
            && (old_pin.len() < 6 || old_pin.len() > 8 || new_pin.len() < 6 || new_pin.len() > 8)
        {
            return Err(ApduError::InvalidPinLength);
        }

        for &c in old_pin.iter().chain(new_pin.iter()) {
            if !c.is_ascii_digit() {
                return Err(ApduError::InvalidPinCharacter);
            }
        }

        let mut apdu = [0u8; 21];
        let total_len = old_pin.len() + new_pin.len();

        apdu[0] = self.cla;
        apdu[1] = 0x24;
        apdu[2] = self.p1;
        apdu[3] = self.p2;
        apdu[4] = total_len as u8;

        apdu[5..5 + old_pin.len()].copy_from_slice(old_pin);
        apdu[5 + old_pin.len()..5 + total_len].copy_from_slice(new_pin);

        Ok(apdu)
    }

    pub fn apdu_len(old_len: usize, new_len: usize) -> usize {
        5 + old_len + new_len
    }
}

/// APDU construction errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApduError {
    /// PIN is too short
    PinTooShort,
    /// PIN is too long  
    PinTooLong,
    /// PIN length is invalid for the PIN type
    InvalidPinLength,
    /// PIN contains non-digit characters
    InvalidPinCharacter,
}

impl core::fmt::Display for ApduError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ApduError::PinTooShort => write!(f, "PIN is too short"),
            ApduError::PinTooLong => write!(f, "PIN is too long"),
            ApduError::InvalidPinLength => write!(f, "Invalid PIN length for this PIN type"),
            ApduError::InvalidPinCharacter => write!(f, "PIN contains invalid characters"),
        }
    }
}

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
    fn test_build_user_pin_apdu_6_digits() {
        let builder = VerifyApduBuilder::user_pin();
        let pin = b"123456";
        let apdu = builder.build(pin).unwrap();

        assert_eq!(apdu[0], 0x00); // CLA
        assert_eq!(apdu[1], 0x20); // INS = VERIFY
        assert_eq!(apdu[2], 0x00); // P1
        assert_eq!(apdu[3], 0x81); // P2 = User PIN
        assert_eq!(apdu[4], 0x06); // Lc = 6
        assert_eq!(&apdu[5..11], b"123456");
    }

    #[test]
    fn test_build_user_pin_apdu_8_digits() {
        let builder = VerifyApduBuilder::user_pin();
        let pin = b"12345678";
        let apdu = builder.build(pin).unwrap();

        assert_eq!(apdu[4], 0x08); // Lc = 8
        assert_eq!(&apdu[5..13], b"12345678");
    }

    #[test]
    fn test_build_admin_pin_apdu() {
        let builder = VerifyApduBuilder::admin_pin();
        let pin = b"12345678";
        let apdu = builder.build(pin).unwrap();

        assert_eq!(apdu[0], 0x00); // CLA
        assert_eq!(apdu[1], 0x20); // INS = VERIFY
        assert_eq!(apdu[2], 0x00); // P1
        assert_eq!(apdu[3], 0x83); // P2 = Admin PIN
        assert_eq!(apdu[4], 0x08); // Lc = 8
        assert_eq!(&apdu[5..13], b"12345678");
    }

    #[test]
    fn test_user_pin_too_short() {
        let builder = VerifyApduBuilder::user_pin();
        let pin = b"12345"; // 5 digits
        assert_eq!(builder.build(pin), Err(ApduError::InvalidPinLength));
    }

    #[test]
    fn test_user_pin_too_long() {
        let builder = VerifyApduBuilder::user_pin();
        let pin = b"123456789"; // 9 digits
        assert_eq!(builder.build(pin), Err(ApduError::InvalidPinLength));
    }

    #[test]
    fn test_admin_pin_wrong_length() {
        let builder = VerifyApduBuilder::admin_pin();

        // Admin PIN must be exactly 8 characters
        assert_eq!(builder.build(b"123456"), Err(ApduError::InvalidPinLength));
        assert_eq!(builder.build(b"1234567"), Err(ApduError::InvalidPinLength));
        assert_eq!(
            builder.build(b"123456789"),
            Err(ApduError::InvalidPinLength)
        );
    }

    #[test]
    fn test_invalid_pin_characters() {
        let builder = VerifyApduBuilder::user_pin();

        // Non-digit characters
        assert_eq!(
            builder.build(b"abcdEF"),
            Err(ApduError::InvalidPinCharacter)
        );
        assert_eq!(
            builder.build(b"1234ab"),
            Err(ApduError::InvalidPinCharacter)
        );
    }

    #[test]
    fn test_build_from_digits() {
        let builder = VerifyApduBuilder::user_pin();
        let digits = [1, 2, 3, 4, 5, 6];
        let apdu = builder.build_from_digits(&digits).unwrap();

        assert_eq!(&apdu[5..11], b"123456");
    }

    #[test]
    fn test_build_from_digits_invalid() {
        let builder = VerifyApduBuilder::user_pin();

        // Digit > 9 is invalid
        assert_eq!(
            builder.build_from_digits(&[1, 2, 10, 4, 5, 6]),
            Err(ApduError::InvalidPinCharacter)
        );
    }

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

    #[test]
    fn test_apdu_len() {
        assert_eq!(VerifyApduBuilder::apdu_len(6), 11); // 5 + 6
        assert_eq!(VerifyApduBuilder::apdu_len(8), 13); // 5 + 8
    }

    #[test]
    fn test_from_template() {
        let builder = VerifyApduBuilder::from_template(0x00, 0x00, 0x81);
        assert!(builder.is_user_pin());

        let builder = VerifyApduBuilder::from_template(0x00, 0x00, 0x83);
        assert!(builder.is_admin_pin());
    }
}
