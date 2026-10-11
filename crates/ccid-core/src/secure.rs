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
///
/// Wire layout of `abPINDataStructure` — the bytes pcscd actually puts on
/// the wire. The host-side source structure (pcsc-lite `reader.h`
/// `PIN_VERIFY_STRUCTURE`) is copied into the CCID payload **verbatim and
/// little-endian** by libccid's `SecurePINVerify` (only `bTimerOut2` and
/// `ulDataLength` are dropped), so the PC/SC field order IS the CCID field
/// order:
///
/// | Offset | Field | Description |
/// |--------|-------|-------------|
/// | 0 | bTimeOut | Timeout in seconds (0 = no timeout) |
/// | 1 | bmFormatString | PIN format and justification |
/// | 2 | bmPINBlockString | PIN block length |
/// | 3 | bmPINLengthFormat | PIN length format |
/// | 4 | wPINMaxExtraDigit LSB | **max** PIN length (see citation below) |
/// | 5 | wPINMaxExtraDigit MSB | **min** PIN length |
/// | 6 | bEntryValidationCondition | Validation trigger |
/// | 7 | bNumberMessage | Number of messages |
/// | 8-9 | wLangId | Language ID (LE) |
/// | 10 | bMsgIndex | Message index |
/// | 11-13 | bTeoPrologue[3] | TPDU prologue (3 bytes!) |
/// | 14+ | abPINApdu | APDU template |
///
/// `wPINMaxExtraDigit` is a little-endian u16 whose value hosts build as
/// `(min << 8) + max` — hence wire byte 4 = max, byte 5 = min.
// PCSC: uint16_t wPINMaxExtraDigit;
// PCSC: /**< 0xXXYY where XX is minimum PIN size in digits,
// PCSC: and YY is maximum PIN size in digits */
// PCSC: uint8_t bTeoPrologue[3]; /**< T=1 block prologue field to use (fill with 00) */
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
        // APDU template starts after the 3-byte bTeoPrologue (offset 14).
        const FIXED_LEN: usize = 14;
        const APDU_LEN: usize = 4;
        if data.len() < FIXED_LEN + APDU_LEN {
            return None;
        }

        let timeout_secs = data[0];
        let format = data[1];
        let pin_block_string = data[2];
        let pin_length_format = data[3];

        // wPINMaxExtraDigit: LE u16, value (min << 8) | max — LSB (wire
        // byte 4) is max, MSB (byte 5) is min.
        let max_len = data[4];
        let min_len = data[5];

        let validation = data[6];
        let number_message = data[7];
        let lang_id = u16::from_le_bytes([data[8], data[9]]);
        let message_index = data[10];
        let teo_prologue = data[11];

        let apdu_start = 14;

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
            message_index,
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
/// Wire layout of `abPINDataStructure` — same derivation as verify
/// (pcsc-lite `PIN_MODIFY_STRUCTURE` copied verbatim LE by libccid's
/// `SecurePINModify`), with one variable-length twist: **libccid omits
/// `bMsgIndex2` when `bNumberMessage == 0` and `bMsgIndex3` when
/// `bNumberMessage < 3`** — everything after those fields shifts left
/// accordingly (bTeoPrologue and the APDU template included).
///
/// | Offset | Field | Description |
/// |--------|-------|-------------|
/// | 0 | bTimeOut | Timeout in seconds (0 = no timeout) |
/// | 1 | bmFormatString | PIN format and justification |
/// | 2 | bmPINBlockString | PIN block length |
/// | 3 | bmPINLengthFormat | PIN length format |
/// | 4 | bInsertionOffsetOld | Offset for old PIN in APDU |
/// | 5 | bInsertionOffsetNew | Offset for new PIN in APDU |
/// | 6 | wPINMaxExtraDigit LSB | **max** PIN length |
/// | 7 | wPINMaxExtraDigit MSB | **min** PIN length |
/// | 8 | bConfirmPIN | Confirmation flags |
/// | 9 | bEntryValidationCondition | Validation trigger |
/// | 10 | bNumberMessage | Number of messages (governs msg2/msg3 presence) |
/// | 11-12 | wLangId | Language ID (LE) |
/// | 13 | bMsgIndex1 | Message index for old PIN prompt |
/// | 14 | bMsgIndex2 | present only if bNumberMessage != 0 |
/// | 15 | bMsgIndex3 | present only if bNumberMessage == 3 |
/// | var | bTeoPrologue[3] | TPDU prologue |
/// | var+3+ | abPINApdu | APDU template |
// PCSC: uint8_t bConfirmPIN; /**< Flags governing need for confirmation of new PIN */
// PCSC: uint8_t bMsgIndex1; /**< index of 1st prompting message */
// PCSC: uint8_t bMsgIndex2; /**< index of 2d prompting message */
// PCSC: uint8_t bMsgIndex3; /**< index of 3d prompting message */
// PCSC: uint8_t bTeoPrologue[3]; /**< T=1 block prologue field to use (fill with 00) */
// PCSC: uint32_t ulDataLength; /**< length of Data to be sent to the ICC */
// LIBCCID: /* Build a CCID block from a PC/SC V2.02.05 Part 10 block */
// LIBCCID: if (15 == b) /* bMsgIndex2 */
// LIBCCID: {
// LIBCCID: /* in CCID the bMsgIndex2 is present only if bNumberMessage != 0 */
// LIBCCID: if (0 == TxBuffer[11])
// LIBCCID: continue;
// LIBCCID: }
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
        if data.len() < 11 {
            return None;
        }

        let timeout_secs = data[0];
        let format = data[1];
        let _pin_block_string = data[2];
        let _pin_length_format = data[3];
        let old_pin_offset = data[4];
        let new_pin_offset = data[5];

        // wPINMaxExtraDigit: LE u16, value (min << 8) | max.
        let max_len = data[6];
        let min_len = data[7];

        let _confirm_pin = data[8];
        let validation = data[9];
        let number_message = data[10];

        // wLangId then bMsgIndex1 at fixed offsets 11-13.
        let msg_index_old = data[13];
        let mut idx = 14usize;

        // Variant tail: msg2 only when number != 0, msg3 only when
        // number >= 3; then the 3-byte prologue, then the APDU.
        let msg_index_new = if number_message != 0 {
            let v = *data.get(idx)?;
            idx += 1;
            v
        } else {
            1
        };
        let msg_index_confirm = if number_message >= 3 {
            let v = *data.get(idx)?;
            idx += 1;
            v
        } else {
            2
        };

        // bTeoPrologue[3]
        let teo_prologue = *data.get(idx)?;
        idx += 3;

        let cla = *data.get(idx)?;
        let ins = *data.get(idx + 1)?;
        let p1 = *data.get(idx + 2)?;
        let p2 = *data.get(idx + 3)?;

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
            msg_index_old,
            msg_index_new,
            msg_index_confirm,
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

    /// Reference-locked §6.1.11 probe: the exact byte shape pcscd puts on
    /// the wire for a 4..8-digit user-PIN VERIFY.
    ///
    /// wPINMaxExtraDigit = (min 4 << 8) + max 8 = 0x0408, stored
    /// little-endian → wire bytes `[0x08, 0x04]` (max FIRST).
    /// bTeoPrologue[3] = 00 00 00, APDU head `00 20 00 81` (VERIFY).
    ///
    /// Citations: pcsc-lite `examples/scardcontrol.c`
    /// (`wPINMaxExtraDigit = (PIN_min_size << 8) + PIN_max_size;`),
    /// `reference/CCID/MacOSX/reader.h` PIN_VERIFY_STRUCTURE, and the
    /// libccid `SecurePINVerify` verbatim-copy loop
    /// (reference/CCID/src/commands.c).
    fn verify_data() -> [u8; 18] {
        [
            30,   // bTimeOut
            0x82, // bmFormatString
            0x00, // bmPINBlockString
            0x00, // bmPINLengthFormat
            0x08, // wPINMaxExtraDigit LSB = max = 8
            0x04, // wPINMaxExtraDigit MSB = min = 4
            0x02, // bEntryValidationCondition
            0x01, // bNumberMessage
            0x09, 0x04, // wLangId = 0x0409 (LE)
            0x00, // bMsgIndex
            0x00, 0x00, 0x00, // bTeoPrologue[3]
            0x00, 0x20, 0x00, 0x81, // abPINApdu head (VERIFY, P2=0x81 user)
        ]
    }

    #[test]
    fn parse_pin_verify_extracts_wire_fields() {
        let p = PinVerifyParams::parse(&verify_data()).expect("parse");
        assert_eq!(p.timeout_secs, 30);
        // min/max MUST come from the max-first wire order — a swap here
        // sends inverted length constraints to the pin-pad UI and a wrong
        // Lc in the reader-built VERIFY APDU.
        assert_eq!(p.min_len, 4);
        assert_eq!(p.max_len, 8);
        assert_eq!(p.pin_type, 0x81);
        assert!(p.is_user_pin());
        assert!(!p.is_admin_pin());
        // APDU head lives after the 3-byte bTeoPrologue (offset 14).
        assert_eq!(p.apdu_template[..4], [0x00, 0x20, 0x00, 0x81]);
        assert_eq!(p.lang_id, 0x0409);
        assert_eq!(p.message_index, 0x00);
        assert_eq!(p.number_message, 0x01);
        assert_eq!(p.validation, 0x02);
    }

    #[test]
    fn parse_pin_verify_rejects_short_data() {
        assert!(PinVerifyParams::parse(&[0u8; 17]).is_none());
        // fixed fields + 3-byte prologue + 4-byte APDU head exactly fits
        let mut d = [0u8; 18];
        d[14..18].copy_from_slice(&[0x00, 0x20, 0x00, 0x81]);
        assert!(PinVerifyParams::parse(&d).is_some());
    }

    /// Reference-locked §6.1.12 probe (full form, bNumberMessage = 3):
    /// wPINMaxExtraDigit wire `[max=8, min=4]`, bConfirmPIN = 0x03,
    /// all three bMsgIndex bytes present, bTeoPrologue[3], APDU at 19.
    /// Same citation chain as `verify_data`.
    fn modify_data_full() -> [u8; 23] {
        let mut d = [0u8; 23];
        d[0] = 30; // bTimeOut
        d[1] = 0x82; // bmFormatString
        d[4] = 0x00; // bInsertionOffsetOld
        d[5] = 0x04; // bInsertionOffsetNew
        d[6] = 0x08; // wPINMaxExtraDigit LSB = max
        d[7] = 0x04; // wPINMaxExtraDigit MSB = min
        d[8] = 0x03; // bConfirmPIN (confirm + current-PIN entry)
        d[9] = 0x02; // bEntryValidationCondition
        d[10] = 3; // bNumberMessage = 3 → msg2 AND msg3 present
        d[11] = 0x09;
        d[12] = 0x04; // wLangId = 0x0409 (LE)
        d[13] = 0x00; // bMsgIndex1
        d[14] = 0x01; // bMsgIndex2
        d[15] = 0x02; // bMsgIndex3
                      // d[16..19] = bTeoPrologue[3] (zero)
        d[19..23].copy_from_slice(&[0x00, 0x24, 0x00, 0x83]); // CHANGE, P2=0x83
        d
    }

    #[test]
    fn parse_pin_modify_full_form() {
        let p = PinModifyParams::parse(&modify_data_full()).expect("parse");
        assert_eq!(p.min_len, 4);
        assert_eq!(p.max_len, 8);
        assert_eq!(p.old_pin_offset, 0x00);
        assert_eq!(p.new_pin_offset, 0x04);
        assert_eq!(p.validation, 0x02);
        assert_eq!(p.msg_index_old, 0x00);
        assert_eq!(p.msg_index_new, 0x01);
        assert_eq!(p.msg_index_confirm, 0x02);
        assert_eq!(p.pin_type, 0x83);
        assert!(p.is_admin_pin());
        assert_eq!(p.apdu_template, [0x00, 0x24, 0x00, 0x83, 16]);
    }

    /// bNumberMessage = 1: libccid omits bMsgIndex3 — the prologue and
    /// APDU shift one byte left (APDU at 18).
    #[test]
    fn parse_pin_modify_number_one_drops_msg3() {
        let mut d = modify_data_full();
        d[10] = 1;
        // bMsgIndex2 kept, bMsgIndex3 dropped: prologue lands at 15, APDU at 18
        let mut compact = [0u8; 22];
        compact[..15].copy_from_slice(&d[..15]);
        compact[15..18].copy_from_slice(&[0x00, 0x00, 0x00]); // prologue
        compact[18..22].copy_from_slice(&[0x00, 0x24, 0x00, 0x81]);
        let p = PinModifyParams::parse(&compact).expect("parse");
        assert_eq!(p.pin_type, 0x81);
        assert_eq!(p.apdu_template[..4], [0x00, 0x24, 0x00, 0x81]);
        assert_eq!(p.msg_index_old, 0x00);
        assert_eq!(p.msg_index_new, 0x01);
    }

    /// bNumberMessage = 0: libccid omits bMsgIndex2 AND bMsgIndex3 —
    /// APDU lands at 17.
    #[test]
    fn parse_pin_modify_number_zero_drops_msg2_and_msg3() {
        let mut d = modify_data_full();
        d[10] = 0;
        let mut compact = [0u8; 21];
        compact[..14].copy_from_slice(&d[..14]);
        compact[14..17].copy_from_slice(&[0x00, 0x00, 0x00]); // prologue
        compact[17..21].copy_from_slice(&[0x00, 0x24, 0x00, 0x81]);
        let p = PinModifyParams::parse(&compact).expect("parse");
        assert_eq!(p.apdu_template[..4], [0x00, 0x24, 0x00, 0x81]);
        assert_eq!(p.min_len, 4);
        assert_eq!(p.max_len, 8);
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
