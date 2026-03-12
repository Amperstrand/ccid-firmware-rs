//! Comprehensive edge case tests for pinpad module
//!
//! These tests cover boundary conditions, error cases, and edge scenarios.

#![cfg(test)]

use ccid_firmware_rs::{
    VerifyApduBuilder, PinBuffer, PinVerifyParams, PinResult, PinEntryContext, PinEntryState,
    ApduError, ButtonId, Keypad, TouchHandler,
};
use embedded_graphics::prelude::*;

// ============================================================================
// PIN Buffer Edge Cases
// ============================================================================

#[test]
fn test_pin_buffer_empty() {
    let buf = PinBuffer::new(8);
    assert!(buf.is_empty());
    assert_eq!(buf.len(), 0);
    assert!(!buf.has_minimum(1));
}

#[test]
fn test_pin_buffer_single_digit() {
    let mut buf = PinBuffer::new(8);
    assert!(buf.push(5));
    assert_eq!(buf.len(), 1);
    assert!(buf.has_minimum(1));
    assert!(!buf.has_minimum(2));
}

#[test]
fn test_pin_buffer_all_digits() {
    let mut buf = PinBuffer::new(10);
    for d in 0..=9 {
        assert!(buf.push(d), "Failed to push digit {}", d);
    }
    assert_eq!(buf.len(), 10);
    
    // Verify all digits stored correctly
    let ascii = buf.to_ascii();
    assert_eq!(&ascii[..10], b"0123456789");
}

#[test]
fn test_pin_buffer_invalid_digit() {
    let mut buf = PinBuffer::new(8);
    
    // Digits 0-9 should work
    assert!(buf.push(0));
    assert!(buf.push(9));
    
    // Digits > 9 should fail
    assert!(!buf.push(10));
    assert!(!buf.push(15));
    assert!(!buf.push(255));
    
    assert_eq!(buf.len(), 2); // Only valid digits should be stored
}

#[test]
fn test_pin_buffer_pop_empty() {
    let mut buf = PinBuffer::new(8);
    assert!(!buf.pop()); // Pop on empty buffer should fail
}

#[test]
fn test_pin_buffer_multiple_clears() {
    let mut buf = PinBuffer::new(8);
    buf.push(1);
    buf.push(2);
    buf.clear();
    buf.clear(); // Second clear should be safe
    assert!(buf.is_empty());
}

#[test]
fn test_pin_buffer_exact_max_len() {
    let mut buf = PinBuffer::new(6);
    
    // Exactly fill buffer
    for d in 1..=6 {
        assert!(buf.push(d));
    }
    
    // Next push should fail
    assert!(!buf.push(7));
    assert_eq!(buf.len(), 6);
}

#[test]
fn test_pin_buffer_to_mask_empty() {
    let buf = PinBuffer::new(8);
    let mask = buf.to_mask();
    
    // All should be zeros for empty buffer
    assert_eq!(mask, [0u8; 16]);
}

#[test]
fn test_pin_buffer_to_mask_partial() {
    let mut buf = PinBuffer::new(8);
    buf.push(1);
    buf.push(2);
    
    let mask = buf.to_mask();
    assert_eq!(mask[0], b'*');
    assert_eq!(mask[1], b'*');
    assert_eq!(mask[2], 0); // Rest should be zeros
}

#[test]
fn test_pin_buffer_max_capacity() {
    // Test with max supported capacity (16 digits)
    let mut buf = PinBuffer::new(20); // Request more than max
    
    // Should be clamped to 16
    for d in 0..16 {
        assert!(buf.push((d % 10) as u8));
    }
    assert_eq!(buf.len(), 16);
}

// ============================================================================
// PIN Entry State Machine Edge Cases
// ============================================================================

#[test]
fn test_state_machine_double_start() {
    let params = PinVerifyParams::default();
    let mut ctx = PinEntryContext::new(params);
    
    ctx.start(0);
    ctx.add_digit(1);
    
    // Start again should reset
    ctx.start(100);
    assert_eq!(ctx.state, PinEntryState::WaitingForPin);
    assert_eq!(ctx.buffer.len(), 0); // Buffer should be cleared
}

#[test]
fn test_state_machine_add_digit_after_submit() {
    let params = PinVerifyParams::default();
    let mut ctx = PinEntryContext::new(params);
    
    ctx.start(0);
    for d in [1, 2, 3, 4, 5, 6] {
        ctx.add_digit(d);
    }
    ctx.submit();
    
    // Adding digit after submit should fail
    assert!(!ctx.add_digit(7));
}

#[test]
fn test_state_machine_backspace_empty() {
    let params = PinVerifyParams::default();
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    
    // Backspace on empty buffer should fail
    assert!(!ctx.backspace());
}

#[test]
fn test_state_machine_submit_empty() {
    let params = PinVerifyParams {
        min_len: 1,
        ..PinVerifyParams::default()
    };
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    
    // Submit empty should fail
    let result = ctx.submit();
    assert_eq!(result, PinResult::InvalidLength);
}

#[test]
fn test_state_machine_submit_exact_min() {
    let params = PinVerifyParams {
        min_len: 6,
        max_len: 8,
        ..PinVerifyParams::default()
    };
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    
    // Enter exactly min digits
    for d in [1, 2, 3, 4, 5, 6] {
        ctx.add_digit(d);
    }
    
    let result = ctx.submit();
    assert_eq!(result, PinResult::Success);
}

#[test]
fn test_state_machine_submit_exact_max() {
    let params = PinVerifyParams {
        min_len: 6,
        max_len: 8,
        ..PinVerifyParams::default()
    };
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    
    // Enter exactly max digits
    for d in [1, 2, 3, 4, 5, 6, 7, 8] {
        ctx.add_digit(d);
    }
    
    let result = ctx.submit();
    assert_eq!(result, PinResult::Success);
}

#[test]
fn test_state_machine_cancel_clears_buffer() {
    let params = PinVerifyParams::default();
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    
    for d in [1, 2, 3, 4, 5, 6, 7, 8] {
        ctx.add_digit(d);
    }
    assert_eq!(ctx.buffer.len(), 8);
    
    ctx.cancel();
    assert_eq!(ctx.buffer.len(), 0);
}

#[test]
fn test_state_machine_timeout_clears_buffer() {
    let params = PinVerifyParams {
        timeout_secs: 10,
        ..PinVerifyParams::default()
    };
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    
    for d in [1, 2, 3, 4, 5, 6] {
        ctx.add_digit(d);
    }
    assert_eq!(ctx.buffer.len(), 6);
    
    // Trigger timeout
    ctx.check_timeout(10000, 1000);
    assert_eq!(ctx.state, PinEntryState::Timeout);
    assert_eq!(ctx.buffer.len(), 0);
}

#[test]
fn test_state_machine_tick_wrapping() {
    let params = PinVerifyParams {
        timeout_secs: 10,
        ..PinVerifyParams::default()
    };
    let mut ctx = PinEntryContext::new(params);
    
    // Start near wrap-around
    ctx.start(0xFFFFFFF0);
    
    // Should not timeout immediately
    assert!(!ctx.check_timeout(0xFFFFFFF5, 1000));
    
    // Should timeout after 10 seconds worth of ticks
    // (accounting for wrap-around)
    assert!(ctx.check_timeout(0xFFFFFFF0 + 10000, 1000));
}

#[test]
fn test_state_machine_result_before_complete() {
    let params = PinVerifyParams::default();
    let ctx = PinEntryContext::new(params);
    
    // Before any operation
    assert!(ctx.result().is_none());
}

#[test]
fn test_state_machine_reset_after_complete() {
    let params = PinVerifyParams::default();
    let mut ctx = PinEntryContext::new(params);
    
    ctx.start(0);
    for d in [1, 2, 3, 4, 5, 6] {
        ctx.add_digit(d);
    }
    ctx.submit();
    
    ctx.reset();
    assert_eq!(ctx.state, PinEntryState::Idle);
    assert_eq!(ctx.buffer.len(), 0);
    assert_eq!(ctx.start_ticks, 0);
}

// ============================================================================
// APDU Builder Edge Cases
// ============================================================================

#[test]
fn test_apdu_builder_empty_pin() {
    let builder = VerifyApduBuilder::user_pin();
    assert_eq!(builder.build(&[]), Err(ApduError::PinTooShort));
}

#[test]
fn test_apdu_builder_exact_boundaries() {
    let builder = VerifyApduBuilder::user_pin();
    
    // 5 digits: too short
    assert_eq!(builder.build(b"12345"), Err(ApduError::InvalidPinLength));
    
    // 6 digits: minimum valid
    assert!(builder.build(b"123456").is_ok());
    
    // 8 digits: maximum valid
    assert!(builder.build(b"12345678").is_ok());
    
    // 9 digits: too long
    assert_eq!(builder.build(b"123456789"), Err(ApduError::InvalidPinLength));
}

#[test]
fn test_apdu_builder_admin_pin_exact() {
    let builder = VerifyApduBuilder::admin_pin();
    
    // Admin PIN must be exactly 8 digits
    assert_eq!(builder.build(b"1234567"), Err(ApduError::InvalidPinLength)); // 7 digits
    assert!(builder.build(b"12345678").is_ok()); // 8 digits
    assert_eq!(builder.build(b"123456789"), Err(ApduError::InvalidPinLength)); // 9 digits
}

#[test]
fn test_apdu_builder_non_ascii_digits() {
    let builder = VerifyApduBuilder::user_pin();
    
    // Test various invalid characters
    let invalid_pins: &[&[u8]] = &[
        b"abcdef",      // letters
        b"1234!@",      // special chars
        b"12 3456",     // space
        b"1234\x0056",  // null byte
        b"1234\xff56",  // high byte
    ];
    
    for pin in invalid_pins {
        assert_eq!(builder.build(pin), Err(ApduError::InvalidPinCharacter), 
            "Expected InvalidPinCharacter for {:?}", pin);
    }
}

#[test]
fn test_apdu_builder_mixed_valid_invalid() {
    let builder = VerifyApduBuilder::user_pin();
    
    // Mix of valid and invalid
    assert_eq!(builder.build(b"1234ab"), Err(ApduError::InvalidPinCharacter));
    assert_eq!(builder.build(b"abcd12"), Err(ApduError::InvalidPinCharacter));
}

#[test]
fn test_apdu_builder_from_template() {
    // Test with custom template
    let builder = VerifyApduBuilder::from_template(0x00, 0x00, 0x81);
    assert!(builder.is_user_pin());
    assert!(!builder.is_admin_pin());
    
    let builder = VerifyApduBuilder::from_template(0x00, 0x00, 0x83);
    assert!(!builder.is_user_pin());
    assert!(builder.is_admin_pin());
    
    // Unknown P2 value
    let builder = VerifyApduBuilder::from_template(0x00, 0x00, 0x82);
    assert!(!builder.is_user_pin());
    assert!(!builder.is_admin_pin());
}

#[test]
fn test_apdu_builder_build_from_digits() {
    let builder = VerifyApduBuilder::user_pin();
    
    // Valid digits
    let apdu = builder.build_from_digits(&[1, 2, 3, 4, 5, 6]).unwrap();
    assert_eq!(&apdu[5..11], b"123456");
    
    // Invalid digit (> 9)
    assert_eq!(builder.build_from_digits(&[1, 2, 3, 4, 5, 10]), 
        Err(ApduError::InvalidPinCharacter));
}

#[test]
fn test_apdu_len_calculation() {
    assert_eq!(VerifyApduBuilder::apdu_len(6), 11); // 5 + 6
    assert_eq!(VerifyApduBuilder::apdu_len(8), 13); // 5 + 8
    assert_eq!(VerifyApduBuilder::apdu_len(0), 5);  // Just header
}

// ============================================================================
// PIN Verify Params Edge Cases
// ============================================================================

#[test]
fn test_parse_params_empty() {
    assert!(PinVerifyParams::parse(&[]).is_none());
}

#[test]
fn test_parse_params_minimal_valid() {
    // Exactly 16 bytes (minimum valid)
    let mut data = [0u8; 16];
    data[12] = 0x00; // CLA
    data[13] = 0x20; // INS = VERIFY
    data[14] = 0x00; // P1
    data[15] = 0x81; // P2 = User PIN
    
    let params = PinVerifyParams::parse(&data).unwrap();
    assert_eq!(params.pin_type, 0x81);
}

#[test]
fn test_parse_params_truncated_apdu() {
    let mut data = [0u8; 15]; // Only 3 bytes for APDU (need 4)
    data[12] = 0x00;
    data[13] = 0x20;
    data[14] = 0x00;
    
    assert!(PinVerifyParams::parse(&data).is_none());
}

#[test]
fn test_parse_params_various_pin_types() {
    let mut data = [0u8; 20];
    data[12] = 0x00;
    data[13] = 0x20;
    data[14] = 0x00;
    
    // User PIN (PW1)
    data[15] = 0x81;
    assert_eq!(PinVerifyParams::parse(&data).unwrap().pin_type, 0x81);
    
    // Admin PIN (PW3)
    data[15] = 0x83;
    assert_eq!(PinVerifyParams::parse(&data).unwrap().pin_type, 0x83);
    
    // Other P2 values
    data[15] = 0x82;
    assert_eq!(PinVerifyParams::parse(&data).unwrap().pin_type, 0x82);
}

#[test]
fn test_parse_params_extreme_lengths() {
    let mut data = [0u8; 20];
    data[12] = 0x00;
    data[13] = 0x20;
    data[14] = 0x00;
    data[15] = 0x81;
    
    // Max = 255, Min = 0
    data[4] = 255;
    data[5] = 0;
    
    let params = PinVerifyParams::parse(&data).unwrap();
    assert_eq!(params.max_len, 255);
    assert_eq!(params.min_len, 0);
}

// ============================================================================
// Keypad/Touch Edge Cases
// ============================================================================

#[test]
fn test_keypad_all_digit_buttons() {
    let keypad = Keypad::new();
    
    // Test each digit button
    for digit in 0..=9 {
        let button = keypad.get_button(ButtonId::Digit(digit));
        assert!(button.is_some(), "Button {} not found", digit);
    }
}

#[test]
fn test_keypad_boundary_points() {
    let keypad = Keypad::new();
    
    // Test exact corner points of button 5
    let button5 = keypad.get_button(ButtonId::Digit(5)).unwrap();
    let x_start = button5.bounds.top_left.x;
    let y_start = button5.bounds.top_left.y;
    
    // Top-left corner (inside)
    assert_eq!(keypad.hit_test(Point::new(x_start + 1, y_start + 1)), ButtonId::Digit(5));
    
    // Just outside top-left
    assert_eq!(keypad.hit_test(Point::new(x_start - 1, y_start - 1)), ButtonId::None);
}

#[test]
fn test_touch_handler_rapid_tap() {
    let mut handler = TouchHandler::new();
    let keypad = Keypad::new();
    
    // Rapid tap: touch and release
    let point = Point::new(
        50 + 100 + 20 + 50, // Button 5 X center
        200 + 100 + 10 + 50, // Button 5 Y center
    );
    
    // First tap
    let _ = handler.process(&keypad, Some(point));
    let result1 = handler.process(&keypad, None);
    
    // Second tap
    let _ = handler.process(&keypad, Some(point));
    let result2 = handler.process(&keypad, None);
    
    // Both should return Digit(5)
    assert_eq!(result1, Some(ButtonId::Digit(5)));
    assert_eq!(result2, Some(ButtonId::Digit(5)));
}

#[test]
fn test_touch_handler_hold() {
    let mut handler = TouchHandler::new();
    let keypad = Keypad::new();
    
    let point = Point::new(100, 250); // Button 1
    
    // Touch and hold
    let _ = handler.process(&keypad, Some(point));
    assert_eq!(handler.pressed(), Some(ButtonId::Digit(1)));
    
    // Keep touching - no result yet
    let result = handler.process(&keypad, Some(point));
    assert_eq!(result, None);
    
    // Still pressing
    assert_eq!(handler.pressed(), Some(ButtonId::Digit(1)));
    
    // Now release
    let result = handler.process(&keypad, None);
    assert_eq!(result, Some(ButtonId::Digit(1)));
}

#[test]
fn test_touch_handler_drag_outside() {
    let mut handler = TouchHandler::new();
    let keypad = Keypad::new();
    
    // Touch on button 1
    let button1_point = Point::new(100, 250);
    let _ = handler.process(&keypad, Some(button1_point));
    
    // Drag outside keypad
    let outside_point = Point::new(10, 10);
    let _ = handler.process(&keypad, Some(outside_point));
    
    // Release
    let result = handler.process(&keypad, None);
    
    // Should return None (last touched button was "None")
    assert_eq!(result, Some(ButtonId::None));
}

// ============================================================================
// Integration Tests - Full Flow
// ============================================================================

#[test]
fn test_full_flow_with_backspace() {
    // Simulate user typing wrong digit, backspace, then correct digit
    let params = PinVerifyParams::default();
    let mut ctx = PinEntryContext::new(params);
    ctx.start(0);
    
    // Type 1234
    for d in [1, 2, 3, 4] {
        ctx.add_digit(d);
    }
    assert_eq!(ctx.buffer.len(), 4);
    
    // Type wrong digit 9
    ctx.add_digit(9);
    assert_eq!(ctx.buffer.len(), 5);
    
    // Backspace
    ctx.backspace();
    assert_eq!(ctx.buffer.len(), 4);
    
    // Type correct digits 56
    ctx.add_digit(5);
    ctx.add_digit(6);
    assert_eq!(ctx.buffer.len(), 6);
    
    // Submit
    let result = ctx.submit();
    assert_eq!(result, PinResult::Success);
    
    // Verify PIN is 123456
    let builder = VerifyApduBuilder::user_pin();
    let ascii = ctx.buffer.to_ascii();
    let apdu = builder.build(&ascii[..6]).unwrap();
    assert_eq!(&apdu[5..11], b"123456");
}

#[test]
fn test_full_flow_touch_entry() {
    // Simulate touch-based PIN entry
    let params = PinVerifyParams::default();
    let mut ctx = PinEntryContext::new(params);
    let keypad = Keypad::new();
    let mut handler = TouchHandler::new();
    
    ctx.start(0);
    
    // Simulate touch on buttons 1, 2, 3, 4, 5, 6
    let digit_points: [(u8, Point); 6] = [
        (1, Point::new(50 + 50, 200 + 50)),   // Button 1
        (2, Point::new(50 + 170, 200 + 50)),  // Button 2
        (3, Point::new(50 + 290, 200 + 50)),  // Button 3
        (4, Point::new(50 + 50, 320 + 50)),   // Button 4
        (5, Point::new(50 + 170, 320 + 50)),  // Button 5
        (6, Point::new(50 + 290, 320 + 50)),  // Button 6
    ];
    
    for (expected_digit, point) in digit_points {
        let _ = handler.process(&keypad, Some(point));
        let result = handler.process(&keypad, None);
        
        assert_eq!(result, Some(ButtonId::Digit(expected_digit)));
        ctx.add_digit(expected_digit);
    }
    
    // Press OK
    let ok_point = Point::new(50 + 290, 440 + 50);
    let _ = handler.process(&keypad, Some(ok_point));
    let result = handler.process(&keypad, None);
    assert_eq!(result, Some(ButtonId::Ok));
    
    // Submit
    let pin_result = ctx.submit();
    assert_eq!(pin_result, PinResult::Success);
}
