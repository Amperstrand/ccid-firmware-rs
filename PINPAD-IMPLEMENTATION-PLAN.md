# CCID Secure PIN Entry Implementation Plan

## Overview

This document outlines the implementation plan for adding secure PIN entry functionality to the STM32F469-DISCO CCID firmware. The implementation enables PIN entry on the device's touchscreen display, keeping the PIN secure from the potentially compromised host computer.

**Device Class:** CCID Class 4 (Advanced PIN Pad with Graphics Display/Touchscreen)

For comprehensive architecture documentation including:
- CCID device class details
- PIN pad architecture models (integrated vs standalone/remote)
- Transaction confirmation (SWYS) patterns
- CCID specification details

See **[docs/PINPAD-ARCHITECTURE.md](docs/PINPAD-ARCHITECTURE.md)**.

## Key Architectural Decisions

### Device Classification

Our STM32F469-DISCO with touchscreen is a **CCID Class 4** device:
- Class 1: Standard reader (no PIN pad)
- Class 2: PIN pad with keypad (no display)
- Class 3: PIN pad with keypad + character LCD
- **Class 4: PIN pad with keypad + graphics display/touchscreen** ← our target

### PIN Pad Models

CCID/PC/SC supports two architectural models:

**Model A - Integrated (Traditional):**
- PIN pad and card reader in same physical device
- APDU constructed and sent internally
- Card response returned to host

**Model B - Standalone/Remote (Supported by CCID spec):**
- PIN pad is a separate device from card reader
- PIN pad captures PIN, returns to host (encrypted or as APDU)
- Host forwards to any card reader
- Useful for high-security environments

**Our implementation supports both models.**

### Transaction Confirmation (SWYS)

Sign What You See (SWYS) is supported for Class 4 readers:
1. Host sends transaction data to display
2. Reader shows data on trusted display
3. User physically confirms on device
4. Only then does PIN entry proceed

This prevents man-in-the-middle attacks on transaction data.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Host Computer                            │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐      │
│  │    GnuPG     │───▶│  CCID Driver │───▶│    USB       │      │
│  │   OpenSC     │    │   (pcscd)    │    │  Protocol    │      │
│  └──────────────┘    └──────────────┘    └──────────────┘      │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ USB (PC_to_RDR_Secure 0x69)
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     STM32F469-DISCO                             │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐      │
│  │  CCID Class  │───▶│ Pinpad Module│───▶│ Smartcard    │      │
│  │  (ccid.rs)   │    │(pinpad.rs)   │    │  Driver      │      │
│  └──────────────┘    └──────────────┘    └──────────────┘      │
│         │                   │                                   │
│         │            ┌──────┴──────┐                           │
│         │            │             │                           │
│         │     ┌──────▼──────┐ ┌────▼─────┐                     │
│         │     │   Display   │ │  Touch   │                     │
│         │     │   (LTDC)    │ │(FT6X06)  │                     │
│         │     └─────────────┘ └──────────┘                     │
│         │                                                       │
│  ┌──────▼──────────────────────────────────────────────────┐   │
│  │              USB OTG FS (endpoints)                     │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ ISO 7816 VERIFY APDU
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                    OpenPGP Smartcard                            │
│  ┌──────────────┐                                               │
│  │ VERIFY APDU  │ ◀── CLA=00 INS=20 P1=00 P2=81 Lc=len PIN     │
│  │   Handler    │                                               │
│  └──────────────┘                                               │
└─────────────────────────────────────────────────────────────────┘
```

## Data Structures

### CCID PIN Verification Data Structure (from CCID spec 6.1.11)

The host sends `PC_to_RDR_Secure` with a PIN Verification Data Structure:

| Offset | Field                   | Size | Description                              |
|--------|-------------------------|------|------------------------------------------|
| 0      | bTimerOut               | 1    | Timeout in seconds (0 = no timeout)      |
| 1      | bmFormatString          | 1    | PIN format flags                         |
| 2      | bmPINBlockString        | 1    | PIN block length                         |
| 3      | bmPINLengthFormat       | 1    | PIN length format                        |
| 4-5    | wPINMaxExtraDigit       | 2    | Max/min PIN length (max:high, min:low)   |
| 6      | bEntryValidationCondition| 1   | Validation condition (OK key, timeout)   |
| 7      | bNumberMessage          | 1    | Number of messages to display            |
| 8-9    | wLangId                 | 2    | Language ID                              |
| 10     | bMsgIndex               | 1    | Message index                            |
| 11     | bTeoPrologue            | 1    | TPDU prologue                            |
| 12+    | abPINApdu               | var  | APDU template (CLA INS P1 P2 [Lc] [Data])|

### bmFormatString Bit Fields

```
Bit 0:    0 = binary, 1 = ASCII
Bit 1:    0 = left justified, 1 = right justified  
Bit 2:    0 = PIN in least significant nibble, 1 = most significant
Bits 3-7: PIN position in the first byte (0-31)
```

For OpenPGP cards, typical values:
- `bmFormatString = 0x82`: ASCII format, left justified, PIN position 2
- This means PIN digits are placed starting at byte position 0 in ASCII

### abPINApdu Template (for OpenPGP)

```
Offset  Field   Value   Description
0       CLA     0x00    Class byte
1       INS     0x20    VERIFY instruction
2       P1      0x00    Parameter 1
3       P2      0x81    Parameter 2 (0x81=User PIN, 0x83=Admin PIN)
4       Lc      0x08    Length of PIN data
5+      Data    --      Placeholder for PIN (filled by reader)
```

## Implementation Status

### Already Implemented ✅

| Component | File | Description |
|-----------|------|-------------|
| PIN Buffer | `src/pinpad/mod.rs` | Secure volatile storage for PIN digits |
| PIN Verify Params | `src/pinpad/mod.rs` | CCID structure parser |
| State Machine | `src/pinpad/state.rs` | Full PIN entry state handling |
| Touchscreen UI | `src/pinpad/ui.rs` | embedded-graphics keypad layout |
| APDU Builder | `src/pinpad/apdu.rs` | VERIFY command construction |

### Not Yet Implemented ❌

| Component | File | Action Needed |
|-----------|------|---------------|
| `bPINSupport` | `src/ccid.rs:165` | Change from `0x00` to `0x03` |
| `wLcdLayout` | `src/ccid.rs:164` | Set touchscreen dimensions |
| `dwFeatures` LCD bit | `src/ccid.rs` | Set bit `0x00040000` |
| `PC_to_RDR_Secure` handler | `src/ccid.rs:415` | Full implementation |

## Implementation Steps

### Phase 1: Enable PIN Support in CCID Descriptor

**Changes to `src/ccid.rs`:**

```rust
// Current (line ~164-165):
0x00, 0x00, // wLcdLayout: 0x0000 (no LCD)
0x00,       // bPINSupport: 0x00 (no PIN support)

// Change to:
0x10, 0x10, // wLcdLayout: 16x16 placeholder for touchscreen
0x03,       // bPINSupport: 0x03 (verify + modify supported)

// Also update dwFeatures (around line ~153):
// Add bit 18 (0x00040000) for LCD support
// Current: 0xB2, 0x07, 0x02, 0x00
// New:     0xB2, 0x07, 0x06, 0x00
```

### Phase 2: Implement PC_to_RDR_Secure Handler

**Changes to `src/ccid.rs`:**

```rust
PC_TO_RDR_SECURE => {
    self.handle_secure(seq);
}

fn handle_secure(&mut self, seq: u8) {
    // 1. Parse PIN verification data structure
    let data = &self.rx_buffer[CCID_HEADER_SIZE..];
    let params = match PinVerifyParams::parse(data) {
        Some(p) => p,
        None => {
            self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            return;
        }
    };
    
    // 2. Create PIN entry context
    // 3. Start PIN entry UI flow
    // 4. Wait for completion (polling or async)
    // 5. Construct VERIFY APDU
    // 6. Send to card
    // 7. Return response to host
}
```

### Phase 3: Integrate State Machine with CCID

Connect existing PIN pad modules to the CCID handler:
1. `handle_secure()` creates `PinEntryContext` with parsed params
2. Context drives touchscreen UI updates via `ui.rs`
3. Touch events feed into state machine via `state.rs`
4. On completion, construct APDU via `apdu.rs`
5. Send to card, return response via CCID DataBlock

### Phase 4: Add SWYS Support (Optional)

For transaction confirmation:
1. Parse transaction data from host
2. Display confirmation screen before PIN entry
3. Wait for physical confirmation button press
4. Only then proceed to PIN entry

## Test Plan

### Unit Tests (host-side)

1. Test PIN verification structure parsing
2. Test VERIFY APDU construction
3. Test PIN format encoding (ASCII vs binary)

### Integration Tests (hardware)

1. Test with GnuPG: `gpg --card-edit`
2. Test with OpenSC: `opensc-explorer`
3. Test with custom Python script using pyscard

### Verification Commands

```bash
# Check descriptor shows PIN support
lsusb -v -d <vid:pid> | grep -A5 bPINSupport

# Check pcscd detects PIN capabilities
pcsc_scan

# Test with OpenSC tools
opensc-tool -l
pkcs15-tool --verify-pin
```

## File Structure

```
ccid-reader/
├── src/
│   ├── main.rs           # Main entry, display/touch init
│   ├── ccid.rs           # CCID protocol + PC_to_RDR_Secure handler
│   ├── pinpad/
│   │   ├── mod.rs        # Module exports, PinBuffer, PinVerifyParams
│   │   ├── ui.rs         # UI rendering with embedded-graphics
│   │   ├── state.rs      # State machine for PIN entry
│   │   └── apdu.rs       # APDU construction
│   └── smartcard.rs      # Card driver (no changes needed)
├── docs/
│   ├── PINPAD-ARCHITECTURE.md    # Comprehensive architecture doc
│   └── SECURITY_AND_BOOTLOADER.md
├── Cargo.toml
└── tests/
    ├── integration_test.rs
    └── comprehensive_test.rs
```

## Dependencies

```toml
[target.'cfg(all(target_arch = "arm", target_os = "none"))'.dependencies]
embedded-graphics = "0.8"
stm32f469i-disc = { path = "../stm32f469i-disc", features = ["framebuffer"] }

[target.'cfg(not(target_arch = "arm"))'.dependencies]
embedded-graphics = "0.8"  # For host-side tests
```

## Response Codes

When PIN entry fails or is cancelled, return appropriate CCID errors:

| Error Code | Meaning                |
|------------|------------------------|
| 0x00       | Success                |
| 0xEF       | PIN cancelled by user  |
| 0xF0       | PIN entry timeout      |
| 0xF6       | ICC protocol not supported |

## Security Considerations

1. **PIN buffer clearing**: Always clear the PIN buffer after use (implemented in `PinBuffer::clear()` and `Drop`)
2. **No logging**: Never log or store PINs
3. **Volatile memory**: Use volatile operations for PIN buffer (`core::ptr::write_volatile`)
4. **Display masking**: Show `****` instead of actual PIN digits
5. **Touch isolation**: Touch coordinates never sent to host, only constructed APDU to card

## References

- **CCID Rev 1.1**: https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf
- **PC/SC Part 10**: https://pcscworkgroup.com/Download/Specifications/pcsc10_v2.02.09.pdf
- **Architecture Details**: [docs/PINPAD-ARCHITECTURE.md](docs/PINPAD-ARCHITECTURE.md)
