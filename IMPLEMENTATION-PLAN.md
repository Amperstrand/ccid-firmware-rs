# OpenPGC Pinpad Implementation - Full Implementation Plan

## Executive Summary

This document outlines the complete implementation plan for adding secure PIN entry functionality to the STM32F469-DISCO CCID firmware for OpenPGP cards.

## Critical Decision: Do We Need Embassy/Async?

### Answer: NO, we don't need Embassy for this use case.

**Reasoning:**

1. **CCID Secure is a Blocking Operation by Design**
   - When the host sends `PC_to_RDR_Secure`, it expects a `RDR_to_PC_DataBlock` response
   - During PIN entry, other USB commands are typically rejected with `SLOT_BUSY`
   - The host waits for the PIN operation to complete before sending new commands

2. **Simple Polling Architecture Works**
   ```rust
   loop {
       // Always poll USB (for abort handling)
       if usb_device.poll(&mut [&mut ccid_class]) {
           // USB event handled
       }
       
       // In PIN entry mode, also handle touch
       if ccid_class.is_pin_entry_active() {
           ccid_class.poll_pin_entry(&mut touch, &mut display);
       }
   }
   ```

3. **What Embassy Would Give Us (that we don't need)**
   - Concurrent task execution → Not needed; PIN entry blocks other operations
   - Interrupt-driven I/O → USB is already interrupt-driven at the hardware level
   - Async timeouts → Can implement with systick counter
   - Efficient waiting → We're always actively polling anyway

4. **Converting to Embassy Would Require**
   - Rewriting USB stack (usb-device is synchronous)
   - Rewriting smartcard UART driver
   - Adding embassy-stm32, embassy-executor, embassy-time dependencies
   - Significant refactoring of ccid.rs

**Decision: Use simple polling loop with state machine**

---

## Implementation Architecture

### State Machine

```
┌───────────────┐
│    IDLE       │ ◀─────────────────────────────────────┐
└───────┬───────┘                                       │
        │ PC_to_RDR_Secure received                     │
        ▼                                               │
┌───────────────┐                                       │
│ PIN_ENTRY     │ ──Timeout──▶ ┌────────────────┐       │
│ (display UI)  │              │ TIMEOUT_ERROR  │───────┤
└───────┬───────┘              └────────────────┘       │
        │                              ▲                │
        │ User presses OK              │ User presses   │
        │ (PIN valid length)           │ Cancel         │
        ▼                              │                │
┌───────────────┐              ┌───────┴────────┐       │
│ SEND_APDU     │              │ CANCELLED      │───────┤
│ (to card)     │              └────────────────┘       │
└───────┬───────┘                                       │
        │ APDU response received                        │
        ▼                                               │
┌───────────────┐                                       │
│ SEND_RESPONSE │ ──────────────────────────────────────┘
│ (to host)     │
└───────────────┘
```

### Module Structure

```
src/
├── main.rs              # Main entry, initialize display/touch, main loop
├── ccid.rs              # CCID class, handle_secure() implementation
├── pinpad/
│   ├── mod.rs           # PinVerifyParams, PinBuffer, PinResult
│   ├── apdu.rs          # VerifyApduBuilder, VerifyResponse
│   ├── ui.rs            # Keypad, Button, draw_pinpad (embedded-graphics)
│   └── state.rs         # PinEntryState (state machine)
├── smartcard.rs         # Smartcard driver (existing)
└── t1_engine.rs         # T=1 protocol engine (existing)
```

---

## Implementation Steps (TDD Approach)

### Step 1: Complete APDU Module Tests ✅ (Already done)
- [x] VerifyApduBuilder for User PIN
- [x] VerifyApduBuilder for Admin PIN  
- [x] VerifyResponse parsing
- [x] Error handling

### Step 2: Complete PIN Module Tests ✅ (Already done)
- [x] PinBuffer push/pop/clear
- [x] PinVerifyParams parsing
- [x] Secure memory clearing

### Step 3: Add State Machine (NEW)
- [ ] Define PinEntryState enum
- [ ] Implement state transitions
- [ ] Add timeout handling

### Step 4: Complete UI Module ✅ (Already done)
- [x] Keypad layout
- [x] Button hit testing
- [x] draw_pinpad function

### Step 5: Integrate Display/Touch in main.rs
- [ ] Add stm32f469i-disc dependency
- [ ] Initialize SDRAM for framebuffer
- [ ] Initialize LCD (LTDC/DSI)
- [ ] Initialize touch (FT6X06 on I2C1)
- [ ] Create framebuffer for embedded-graphics

### Step 6: Wire CCID Secure to Pinpad
- [ ] Modify handle_secure() to use state machine
- [ ] Poll touch in main loop when in PIN entry mode
- [ ] Send VERIFY APDU to card on completion
- [ ] Return response to host

### Step 7: Error Handling
- [ ] Timeout → CCID_ERR_PIN_TIMEOUT (0xF0)
- [ ] Cancel → CCID_ERR_PIN_CANCELLED (0xEF)
- [ ] Invalid length → display error message

---

## Key Code Changes

### main.rs Changes

```rust
// Add after smartcard UART initialization:

// Initialize SDRAM for framebuffer
let sdram = Sdram::new(/* ... */);
let fb_buffer: &'static mut [u16] = unsafe { /* ... */ };

// Initialize LCD
let (mut display_ctrl, _) = lcd::init_display_full(/* ... */);

// Initialize touch I2C
let mut i2c = touch::init_i2c(dp.I2C1, pb8, pb9, &mut rcc);
let mut touch_ctrl = touch::init_ft6x06(&i2c, ts_int);

// Main loop with PIN entry
loop {
    usb_device.poll(&mut [&mut ccid_class]);
    
    // Handle PIN entry if active
    if ccid_class.is_pin_entry_active() {
        ccid_class.poll_pin_entry(&mut i2c, &mut touch_ctrl, &mut fb);
    }
}
```

### ccid.rs Changes

```rust
// Add to CcidClass struct:
pin_entry_state: Option<PinEntryState>,
pin_buffer: PinBuffer,
pin_params: Option<PinVerifyParams>,

// Add methods:
pub fn is_pin_entry_active(&self) -> bool { ... }
pub fn poll_pin_entry(&mut self, i2c, touch, display) { ... }
pub fn start_pin_entry(&mut self, params: PinVerifyParams) { ... }
```

---

## Testing Strategy

### Host-side Unit Tests
```bash
cargo test --lib
```

### Hardware Integration Tests
1. **Python test script** (test_ccid_apdu.py):
   - Connect to reader via pcsc
   - Send PC_to_RDR_Secure with test APDU template
   - Verify response

2. **GnuPG test**:
   ```bash
   gpg --card-edit
   > admin
   > passwd
   # Should trigger PIN entry on device
   ```

3. **OpenSC test**:
   ```bash
   opensc-explorer
   > verify CHV1
   # Should trigger PIN entry on device
   ```

---

## Dependencies

```toml
[target.'cfg(all(target_arch = "arm", target_os = "none"))'.dependencies]
stm32f469i-disc = { path = "../stm32f469i-disc", features = ["framebuffer"] }
embedded-graphics = "0.8"
ft6x06 = { git = "https://github.com/DougAnderson444/ft6x06" }
```

---

## Pinout Reference

| Function | Pin | Port | AF |
|----------|-----|------|----|
| Touch SDA | PB9 | I2C1 | 4 |
| Touch SCL | PB8 | I2C1 | 4 |
| Touch INT | PC1 | GPIO | - |
| LCD Reset | PH7 | GPIO | - |
| Smartcard IO | PA2 | USART2 | 7 |
| Smartcard CLK | PA4 | USART2 | 7 |
| USB DM | PA11 | OTG_FS | 10 |
| USB DP | PA12 | OTG_FS | 10 |

---

## Timeline

| Step | Estimated Time | Dependencies |
|------|----------------|--------------|
| State machine | 1 hour | None |
| main.rs integration | 2 hours | stm32f469i-disc |
| CCID wiring | 2 hours | State machine |
| Testing | 2 hours | All above |
| Debug/fix | 2 hours | Testing |

**Total: ~9 hours**

---

## Security Checklist

- [x] PIN buffer uses volatile operations for clearing
- [x] PIN is never logged or stored persistently
- [x] Display shows masked PIN (****)
- [ ] PIN buffer is cleared on cancel/timeout
- [ ] PIN buffer is cleared after APDU transmission
- [ ] No PIN leakage in error responses
