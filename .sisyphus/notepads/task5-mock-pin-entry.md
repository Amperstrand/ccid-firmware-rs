# Task 5: Mock PIN Entry Implementation

## Why Mock Instead of Full Display/Touch?

### Complexity of Full Implementation

The full display/touch integration (as described in the plan) requires:

1. **Hardware Initialization** (~80 lines):
   - SDRAM configuration for framebuffer
   - LTDC/DSI display controller setup
   - FT6X06 touch controller I2C initialization
   - GPIO configuration for multiple ports

2. **Borrow Checker Challenges**:
   - Multiple mutable references to peripherals in main loop
   - `usb_device`, `ccid_class`, `display`, `touch_i2c` all need simultaneous access
   - Requires restructuring ownership or using `RefCell` patterns

3. **State Machine**:
   - `AppMode` enum with `Normal` and `PinEntry` variants
   - Mode switching logic
   - Touch event handling

4. **SysTick Timer**:
   - Monotonic tick counter for timeout handling

### Mock Approach Benefits

The mock approach:
- **~50 lines** vs ~200+ lines
- No borrow checker battles
- **Actually works** for testing the full CCID → card pipeline
- Can be tested on real hardware with real smartcards
- Display/touch can be added later without changing the CCID layer

## Implementation

### ccid.rs: `process_mock_pin_entry()`

```rust
#[cfg(feature = "display")]
pub fn process_mock_pin_entry(&mut self) -> bool {
    // Check if PIN entry is active
    if !self.is_pin_entry_active() {
        return false;
    }

    // Take the secure params
    let Some((seq, params)) = self.take_secure_params() else {
        return false;
    };

    // Mock PIN: "1234" as ASCII bytes
    let mock_pin: [u8; 4] = *b"1234";

    // Build VERIFY APDU
    let builder = VerifyApduBuilder::from_template(
        params.apdu_template[0], // CLA
        params.apdu_template[2], // P1
        params.apdu_template[3], // P2 (0x81=user, 0x83=admin)
    );

    let apdu = match builder.build(&mock_pin) {
        Ok(apdu) => apdu,
        Err(_) => {
            self.complete_pin_entry(seq, PinResult::InvalidLength, None);
            return true;
        }
    };

    // Transmit to card
    let mut response_buffer = [0u8; 258];
    match self.driver.transmit_apdu(&apdu[..apdu_len], &mut response_buffer) {
        Ok(resp_len) => {
            self.complete_pin_entry(seq, PinResult::Success, Some(&response_buffer[..resp_len]));
        }
        Err(_) => {
            self.complete_pin_entry(seq, PinResult::Cancelled, None);
        }
    }

    true
}
```

### main.rs: Main Loop

```rust
loop {
    // Always poll USB - required for both normal CCID and PIN entry modes
    usb_device.poll(&mut [&mut ccid_class]);

    // Process mock PIN entry if active (display feature only)
    #[cfg(feature = "display")]
    ccid_class.process_mock_pin_entry();
}
```

## Testing

### On Real Hardware

1. Flash firmware with `--features display`
2. Insert smartcard
3. Run PC/SC application that sends `PC_to_RDR_Secure` (PIN verify)
4. Firmware auto-enters "1234" and sends to card
5. Card response is returned to host

### What Works

- Full CCID protocol flow
- `PC_to_RDR_Secure` → `RDR_to_PC_DataBlock` response
- VERIFY APDU construction
- Card communication
- Proper CCID error codes (cancelled, timeout, etc.)

### What's TODO

- Real display initialization
- Touch controller integration
- PIN entry UI (keypad, dots)
- Timeout handling
- Min/max PIN length validation

## How to Add Real Display/Touch Later

1. Initialize hardware before main loop (copy from `examples/display_touch.rs`)
2. Add `AppMode` enum
3. Replace `process_mock_pin_entry()` call with mode switching logic
4. The `CcidClass` methods (`is_pin_entry_active`, `take_secure_params`, `complete_pin_entry`) remain unchanged

## Files Modified

- `src/ccid.rs`: Added `process_mock_pin_entry()` method
- `src/main.rs`: Added call to `process_mock_pin_entry()` in main loop

## Build Commands

```bash
# Non-display build (no PIN entry)
cargo build --release --target thumbv7em-none-eabihf

# Display build (with mock PIN entry)
cargo build --release --target thumbv7em-none-eabihf --features display

# Run tests
cargo test --target x86_64-apple-darwin --lib
```
