//! PC_to_RDR_Secure wire structures (CCID Rev 1.1 §6.1.11 PIN verify /
//! §6.1.12 PIN modify) — moved from the STM32 `pinpad` module so the
//! shared handler core owns the secure-Messaging surface (DRY phase 2,
//! issue #121: one place to be right for these wire formats).
//!
//! The APDU builders (`VerifyApduBuilder`/`ModifyApduBuilder`) and the
//! pinpad UI state machine stay firmware-side — they consume these types
//! but carry no CCID wire bytes.

/// PIN operation result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl PinResult {
    /// `&'static str` name for defmt-style logging (the firmware's ARM
    /// builds have no `Debug` formatting over defmt).
    pub fn as_str(self) -> &'static str {
        match self {
            PinResult::Success => "Success",
            PinResult::Cancelled => "Cancelled",
            PinResult::Timeout => "Timeout",
            PinResult::InvalidLength => "InvalidLength",
        }
    }
}

/// Secure (PIN) session state of the handler core. Armed by an accepted
/// PC_to_RDR_Secure; the transport wrapper consumes it as an event and
/// completes it via [`crate::handler::CcidHandlerCore::complete_pin_entry`].
#[derive(Debug, Clone, Copy)]
pub enum SecureState {
    Idle,
    WaitingForPinVerify { seq: u8, params: PinVerifyParams },
    WaitingForPinModify { seq: u8, params: PinModifyParams },
}

/// PIN verification parameters extracted from CCID message (§6.1.11)
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

        let min_len = data[4];
        let max_len = data[5];

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

        let min_len = data[6];
        let max_len = data[7];

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The SeedKeeper-style §6.1.11 probe from the firmware suite — pins
    /// the wire offsets the shared core now owns.
    fn verify_data() -> [u8; 17] {
        [
            30,   // bTimerOut
            0x82, // bmFormatString
            0x00, // bmPINBlockString
            0x00, // bmPINLengthFormat
            6,    // wPINMaxExtraDigit lo = min
            8,    // wPINMaxExtraDigit hi = max
            0x02, // bEntryValidationCondition
            1,    // bNumberMessage
            0x09, 0x04, // wLangId
            0,    // bMsgIndex
            0,    // bTeoPrologue
            0x00, 0x20, 0x00, 0x81, // APDU head
            0x08, // Lc
        ]
    }

    #[test]
    fn parse_pin_verify_extracts_wire_fields() {
        let p = PinVerifyParams::parse(&verify_data()).expect("parse");
        assert_eq!(p.timeout_secs, 30);
        assert_eq!(p.min_len, 6);
        assert_eq!(p.max_len, 8);
        assert_eq!(p.pin_type, 0x81);
        assert!(p.is_user_pin());
        assert!(!p.is_admin_pin());
        assert_eq!(p.apdu_template[..4], [0x00, 0x20, 0x00, 0x81]);
        assert_eq!(p.lang_id, 0x0409);
    }

    #[test]
    fn parse_pin_verify_rejects_short_data() {
        assert!(PinVerifyParams::parse(&[0u8; 15]).is_none());
        // header fits but the APDU head is cut
        let mut d = [0u8; 16];
        d[12..16].copy_from_slice(&[0x00, 0x20, 0x00, 0x81]);
        assert!(PinVerifyParams::parse(&d).is_some());
    }

    #[test]
    fn parse_pin_modify_extracts_wire_fields() {
        let mut d = [0u8; 21];
        d[0] = 30;
        d[4] = 5; // old offset
        d[5] = 13; // new offset
        d[6] = 6;
        d[7] = 8;
        d[8] = 0x02;
        d[16..20].copy_from_slice(&[0x00, 0x24, 0x00, 0x83]);
        let p = PinModifyParams::parse(&d).expect("parse");
        assert_eq!(p.min_len, 6);
        assert_eq!(p.max_len, 8);
        assert_eq!(p.old_pin_offset, 5);
        assert_eq!(p.new_pin_offset, 13);
        assert_eq!(p.pin_type, 0x83);
        assert!(p.is_admin_pin());
        assert_eq!(p.apdu_template, [0x00, 0x24, 0x00, 0x83, 16]);
    }

    #[test]
    fn parse_pin_modify_rejects_short_data() {
        assert!(PinModifyParams::parse(&[0u8; 19]).is_none());
    }

    #[test]
    fn pin_buffer_bounds_and_mask() {
        let mut buf = PinBuffer::new(3);
        assert!(buf.is_empty());
        assert!(buf.push(1));
        assert!(buf.push(2));
        assert!(buf.push(3));
        assert!(!buf.push(4));
        assert!(!buf.push(10));
        assert_eq!(buf.len(), 3);
        assert_eq!(&buf.to_ascii()[..3], b"123");
        assert_eq!(&buf.to_mask()[..3], b"***");
        assert!(buf.pop());
        assert_eq!(buf.len(), 2);
        buf.clear();
        assert!(buf.is_empty());
    }

    #[test]
    fn pin_buffer_clamps_to_16() {
        let mut buf = PinBuffer::new(20);
        for d in 0..16u8 {
            assert!(buf.push(d % 10));
        }
        assert_eq!(buf.len(), 16);
    }

    #[test]
    fn pin_result_str_names() {
        assert_eq!(PinResult::Success.as_str(), "Success");
        assert_eq!(PinResult::Cancelled.as_str(), "Cancelled");
        assert_eq!(PinResult::Timeout.as_str(), "Timeout");
        assert_eq!(PinResult::InvalidLength.as_str(), "InvalidLength");
    }
}
