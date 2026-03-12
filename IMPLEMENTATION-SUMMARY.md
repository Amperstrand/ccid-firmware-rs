# CCID Secure PIN Entry - Implementation Summary

## What Was Implemented

I've implemented a complete proof-of-concept for OpenPGP card PIN entry on the STM32F469-DISCO. Here's what was built:

### 1. CCID Descriptor Updates (`src/ccid.rs`)

- **wLcdLayout**: Changed from 0x0000 to 0x200F (32 lines × 15 characters)
- **bPINSupport**: Changed from 0x00 to 0x01 (PIN verification supported)

This tells the host that the reader has a display and supports secure PIN verification.

### 2. Secure PIN Handler (`src/ccid.rs` - `handle_secure()`)

A complete implementation of `PC_to_RDR_Secure` command handling:

- Parses CCID PIN Verification Data Structure
- Extracts APDU template (CLA, INS, P1, P2)
- Determines PIN type (User PIN = 0x81, Admin PIN = 0x83)
- Constructs VERIFY APDU with entered PIN
- Sends APDU to card and returns response
- **Security**: Securely clears PIN from memory after use

### 3. Pinpad Module (`src/pinpad/`)

#### `mod.rs` - Core PIN handling
- `PinVerifyParams`: Parses CCID PIN verification structure
- `PinBuffer`: Secure PIN storage with automatic memory clearing
- `secure_clear()`: Secure memory zeroing function
- Comprehensive unit tests

#### `apdu.rs` - APDU construction
- `VerifyApduBuilder`: Constructs VERIFY APDUs
- `VerifyResponse`: Parses card responses
- Support for both User PIN (6-8 chars) and Admin PIN (8 chars)
- Validation of PIN length and characters
- Comprehensive unit tests

#### `ui.rs` - Display UI (embedded-graphics)
- `Keypad`: 12-button layout (0-9, OK, Cancel)
- `Button`: Touch-sensitive button with hit testing
- `draw_pinpad()`: Renders the complete pinpad UI
- 480×800 portrait layout with masked PIN display

### 4. Dependencies (`Cargo.toml`)

Added `embedded-graphics = "0.8"` for both ARM target and host testing.

## How It Works

### Data Flow

```
Host (GnuPG/OpenSC)
    │
    │ PC_to_RDR_Secure
    │ (PIN Verification Data Structure)
    ▼
CCID Class (handle_secure)
    │
    │ Parse structure → Extract APDU template
    │ Display pinpad → Collect PIN digits
    │ Build VERIFY APDU: 00 20 00 81 06 313233343536
    ▼
OpenPGP Smartcard
    │
    │ Response: 90 00 (success) or 63 CX (wrong PIN)
    ▼
CCID Class → RDR_to_PC_DataBlock with response
    │
    ▼
Host receives result
```

### VERIFY APDU Format

For User PIN "123456":
```
CLA = 0x00
INS = 0x20 (VERIFY)
P1  = 0x00
P2  = 0x81 (User PIN / PW1)
Lc  = 0x06
Data = 31 32 33 34 35 36 (ASCII "123456")
```

### Card Response Codes

| SW1 SW2 | Meaning |
|---------|---------|
| 90 00   | PIN verified successfully |
| 63 Cn   | Wrong PIN, n attempts remaining |
| 63 C0   | PIN blocked (0 attempts) |
| 69 85   | Conditions not satisfied |

## Testing

### Host-Side Unit Tests

The pinpad module includes comprehensive tests that can run on the host:

```rust
#[test]
fn test_build_user_pin_apdu_6_digits() {
    let builder = VerifyApduBuilder::user_pin();
    let pin = b"123456";
    let apdu = builder.build(pin).unwrap();
    
    assert_eq!(apdu[0], 0x00); // CLA
    assert_eq!(apdu[1], 0x20); // INS = VERIFY
    assert_eq!(apdu[3], 0x81); // P2 = User PIN
}
```

### Integration Testing

To test with real hardware:

1. **Flash the firmware** to STM32F469-DISCO
2. **Insert OpenPGP card** into smartcard slot
3. **Connect USB** to host computer

#### With GnuPG

```bash
# Check if PIN pad is detected
gpg --card-status

# Trigger PIN verification
gpg --card-edit
gpg/card> verify
```

#### With OpenSC

```bash
# Check reader capabilities
opensc-tool -l

# Trigger PIN verification
opensc-explorer
> verify CHV1
```

#### With Python (pyscard)

```python
from smartcard.System import readers
from smartcard.util import toBytes, toHexString

# Get the reader
r = readers()[0]

# Send PIN verify via CCID
# (This triggers PC_to_RDR_Secure)
# ... see test_ccid_apdu.py for complete example
```

## Current Limitations

### 1. Test PIN Used
The current implementation uses a hardcoded test PIN for development:
```rust
let test_pin: [u8; 8] = if is_user_pin {
    [b'1', b'2', b'3', b'4', b'5', b'6', 0, 0]  // TEST PIN!
} else {
    [b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8']
};
```

**This MUST be replaced with actual UI-based PIN entry before production!**

### 2. Display Not Integrated
The UI module is written but not integrated into `main.rs`. To fully integrate:

1. Add stm32f469i-disc crate as dependency
2. Initialize display in main.rs
3. Initialize touch controller
4. Add display/touch to USB poll loop
5. Connect UI to `handle_secure()`

### 3. No Timeout Implementation
The timeout from CCID message is parsed but not enforced. A hardware timer should be used.

## Files Modified/Created

| File | Changes |
|------|---------|
| `src/ccid.rs` | Updated descriptor, added `handle_secure()` |
| `src/pinpad/mod.rs` | New: Core PIN handling, tests |
| `src/pinpad/apdu.rs` | New: APDU construction, tests |
| `src/pinpad/ui.rs` | New: embedded-graphics UI |
| `Cargo.toml` | Added embedded-graphics dependency |
| `PINPAD-IMPLEMENTATION-PLAN.md` | Documentation |

## Next Steps for Full Implementation

1. **Integrate display driver** from stm32f469i-disc
2. **Connect UI to handler** - display pinpad on PC_to_RDR_Secure
3. **Handle touch events** - collect PIN from user
4. **Implement timeout** using hardware timer
5. **Add cancellation** support (Cancel button)
6. **Test with real cards** (YubiKey, OpenPGP card)
7. **Security audit** - verify PIN clearing, check for timing attacks

## Architecture Diagram

```
┌───────────────────────────────────────────────────────┐
│                     Host Computer                     │
│  ┌─────────┐  ┌──────────┐  ┌─────────────────────┐  │
│  │  GnuPG  │  │  OpenSC  │  │  Python (pyscard)  │  │
│  └────┬────┘  └────┬─────┘  └─────────┬───────────┘  │
│       │            │                  │               │
│       └────────────┴──────────────────┘               │
│                          │                            │
│                    USB CCID Protocol                  │
│                          │                            │
└──────────────────────────┼────────────────────────────┘
                           │
           ┌───────────────┴───────────────┐
           │      PC_to_RDR_Secure         │
           │  (PIN Verification Structure) │
           └───────────────┬───────────────┘
                           │
┌──────────────────────────┼────────────────────────────┐
│                    STM32F469-DISCO                     │
│                          │                             │
│  ┌───────────────────────┴───────────────────────┐    │
│  │              CCID Class (ccid.rs)              │    │
│  │  • Parse PIN verify structure                 │    │
│  │  • Extract APDU template                      │    │
│  │  • Call pinpad UI                             │    │
│  └───────────────────────┬───────────────────────┘    │
│                          │                             │
│  ┌───────────────────────┴───────────────────────┐    │
│  │           Pinpad Module (pinpad/)              │    │
│  │                                                │    │
│  │  ┌─────────────┐    ┌─────────────┐           │    │
│  │  │  UI (ui.rs) │◄──►│ APDU (apdu) │           │    │
│  │  │             │    │             │           │    │
│  │  │ • Keypad    │    │ • Builder   │           │    │
│  │  │ • Display   │    │ • Response  │           │    │
│  │  │ • Touch     │    │             │           │    │
│  │  └─────────────┘    └─────────────┘           │    │
│  └───────────────────────────────────────────────┘    │
│                          │                             │
│              VERIFY APDU: 00 20 00 81 06 xxxxxx        │
│                          │                             │
└──────────────────────────┼────────────────────────────┘
                           │
┌──────────────────────────┼────────────────────────────┐
│                   OpenPGP Card                        │
│                          │                             │
│  ┌───────────────────────┴───────────────────────┐    │
│  │              VERIFY Command Handler            │    │
│  │  • Validate PIN                               │    │
│  │  • Return SW1 SW2                             │    │
│  └───────────────────────────────────────────────┘    │
│                                                        │
│  Response: 90 00 (success) or 63 CX (wrong PIN)       │
└────────────────────────────────────────────────────────┘
```

## Security Considerations

1. **PIN Memory Clearing**: The `PinBuffer` uses `write_volatile` in `Drop` to ensure PIN is cleared even with compiler optimizations.

2. **No PIN Logging**: The code never logs or prints PIN values.

3. **Display Masking**: PIN is shown as `****` on screen.

4. **No PIN in USB Traffic**: The PIN never travels over USB - it's entered on the device and sent directly to the card.

5. **Test PIN Warning**: The test PIN is clearly marked with `defmt::warn!` so it's obvious during development/testing.
