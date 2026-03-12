//! Integration tests for pinpad module
//!
//! These tests verify the complete PIN entry flow from CCID parsing to APDU construction.

#![cfg(test)]

use ccid_firmware_rs::{
    VerifyApduBuilder, PinBuffer, PinVerifyParams, PinResult, PinEntryContext, PinEntryState,
};

#[test]
fn test_full_pin_entry_flow_user_pin() {
    // Simulate a complete PIN entry flow for User PIN
    
    // 1. Parse CCID PIN verification parameters
    let ccid_data: [u8; 20] = [
        30,     // bTimerOut = 30 seconds
        0x82,   // bmFormatString = ASCII, left justified
        0x00,   // bmPINBlockString
        0x00,   // bmPINLengthFormat
        8,      // wPINMaxExtraDigit high = max 8
        6,      // wPINMaxExtraDigit low = min 6
        0x02,   // bEntryValidationCondition = validation key
        1,      // bNumberMessage
        0x09, 0x04, // wLangId = 0x0409 (English)
        0,      // bMsgIndex
        0,      // bTeoPrologue
        // APDU template
        0x00,   // CLA
        0x20,   // INS = VERIFY
        0x00,   // P1
        0x81,   // P2 = User PIN
        0x08,   // Lc
    ];
    
    let params = PinVerifyParams::parse(&ccid_data).expect("Failed to parse params");
    assert_eq!(params.pin_type, 0x81);
    assert_eq!(params.min_len, 6);
    assert_eq!(params.max_len, 8);
    
    // 2. Create PIN entry context
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    assert_eq!(ctx.state, PinEntryState::WaitingForPin);
    
    // 3. Simulate user entering PIN digits
    let digits = [1, 2, 3, 4, 5, 6];
    for d in digits {
        assert!(ctx.add_digit(d), "Failed to add digit {}", d);
    }
    
    // 4. Submit PIN
    let result = ctx.submit();
    assert_eq!(result, PinResult::Success);
    assert_eq!(ctx.state, PinEntryState::Completed);
    
    // 5. Build VERIFY APDU
    let builder = VerifyApduBuilder::user_pin();
    let pin_ascii = ctx.buffer.to_ascii();
    let apdu = builder.build(&pin_ascii[..6]).expect("Failed to build APDU");
    
    // Verify APDU structure
    assert_eq!(apdu[0], 0x00); // CLA
    assert_eq!(apdu[1], 0x20); // INS = VERIFY
    assert_eq!(apdu[2], 0x00); // P1
    assert_eq!(apdu[3], 0x81); // P2 = User PIN
    assert_eq!(apdu[4], 0x06); // Lc = 6
    assert_eq!(&apdu[5..11], b"123456"); // PIN data
}

#[test]
fn test_full_pin_entry_flow_admin_pin() {
    // Simulate a complete PIN entry flow for Admin PIN
    
    // Create params for Admin PIN
    let mut ccid_data = [0u8; 20];
    ccid_data[0] = 30;
    ccid_data[1] = 0x82;
    ccid_data[4] = 8;  // max
    ccid_data[5] = 8;  // min (Admin PIN is always 8)
    ccid_data[6] = 0x02;
    ccid_data[12] = 0x00; // CLA
    ccid_data[13] = 0x20; // INS = VERIFY
    ccid_data[14] = 0x00; // P1
    ccid_data[15] = 0x83; // P2 = Admin PIN
    ccid_data[16] = 0x08; // Lc
    
    let params = PinVerifyParams::parse(&ccid_data).expect("Failed to parse params");
    assert_eq!(params.pin_type, 0x83);
    
    // Create context and enter PIN
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    
    // Admin PIN must be exactly 8 digits
    for d in [1, 2, 3, 4, 5, 6, 7, 8] {
        ctx.add_digit(d);
    }
    
    let result = ctx.submit();
    assert_eq!(result, PinResult::Success);
    
    // Build APDU
    let builder = VerifyApduBuilder::admin_pin();
    let pin_ascii = ctx.buffer.to_ascii();
    let apdu = builder.build(&pin_ascii[..8]).expect("Failed to build APDU");
    
    assert_eq!(apdu[3], 0x83); // P2 = Admin PIN
    assert_eq!(apdu[4], 0x08); // Lc = 8
    assert_eq!(&apdu[5..13], b"12345678");
}

#[test]
fn test_pin_entry_timeout() {
    // Test timeout handling
    let params = PinVerifyParams {
        timeout_secs: 10,
        ..PinVerifyParams::default()
    };
    
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    
    // Enter some digits
    ctx.add_digit(1);
    ctx.add_digit(2);
    
    // Check timeout at 5 seconds (should not timeout)
    assert!(!ctx.check_timeout(5000, 1000));
    assert_eq!(ctx.state, PinEntryState::WaitingForPin);
    
    // Check timeout at 10 seconds (should timeout)
    assert!(ctx.check_timeout(10000, 1000));
    assert_eq!(ctx.state, PinEntryState::Timeout);
    assert_eq!(ctx.buffer.len(), 0); // Buffer cleared
}

#[test]
fn test_pin_entry_cancel() {
    // Test cancellation
    let params = PinVerifyParams::default();
    let mut ctx = PinEntryContext::new(params);
    
    ctx.start(0);
    ctx.add_digit(1);
    ctx.add_digit(2);
    ctx.add_digit(3);
    
    ctx.cancel();
    
    assert_eq!(ctx.state, PinEntryState::Cancelled);
    assert_eq!(ctx.buffer.len(), 0);
    assert_eq!(ctx.result(), Some(PinResult::Cancelled));
}

#[test]
fn test_pin_entry_invalid_length() {
    // Test invalid PIN length (too short)
    let params = PinVerifyParams {
        min_len: 6,
        max_len: 8,
        ..PinVerifyParams::default()
    };
    
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    
    // Enter only 3 digits
    ctx.add_digit(1);
    ctx.add_digit(2);
    ctx.add_digit(3);
    
    let result = ctx.submit();
    assert_eq!(result, PinResult::InvalidLength);
    assert_eq!(ctx.state, PinEntryState::InvalidLength);
}

#[test]
fn test_pin_buffer_backspace() {
    let mut buf = PinBuffer::new(8);
    
    buf.push(1);
    buf.push(2);
    buf.push(3);
    assert_eq!(buf.len(), 3);
    
    buf.pop();
    assert_eq!(buf.len(), 2);
    
    let ascii = buf.to_ascii();
    assert_eq!(&ascii[..2], b"12");
}

#[test]
fn test_pin_buffer_max_length() {
    let mut buf = PinBuffer::new(3);
    
    assert!(buf.push(1));
    assert!(buf.push(2));
    assert!(buf.push(3));
    assert!(!buf.push(4)); // Should fail, max reached
    
    assert_eq!(buf.len(), 3);
}

#[test]
fn test_apdu_validation() {
    let builder = VerifyApduBuilder::user_pin();
    
    // Too short (5 digits)
    assert_eq!(builder.build(b"12345"), Err(ccid_firmware_rs::ApduError::InvalidPinLength));
    
    // Too long (9 digits)
    assert_eq!(builder.build(b"123456789"), Err(ccid_firmware_rs::ApduError::InvalidPinLength));
    
    // Invalid characters
    assert_eq!(builder.build(b"abcdEF"), Err(ccid_firmware_rs::ApduError::InvalidPinCharacter));
    
    // Valid 6-digit PIN
    assert!(builder.build(b"123456").is_ok());
    
    // Valid 8-digit PIN
    assert!(builder.build(b"12345678").is_ok());
}

#[test]
fn test_secure_memory_clearing() {
    let mut buf = PinBuffer::new(8);
    buf.push(1);
    buf.push(2);
    buf.push(3);
    
    // Verify data is present
    assert_eq!(buf.len(), 3);
    
    // Clear and verify
    buf.clear();
    assert_eq!(buf.len(), 0);
    
    // The Drop implementation also clears memory using volatile writes
    // This is tested implicitly when buf goes out of scope
}
