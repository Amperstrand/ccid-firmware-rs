# CCID Secure PIN Pad Architecture

This document consolidates research on CCID PIN pad architecture, device classes, and implementation patterns for the STM32F469-DISCO firmware.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [CCID Device Classes](#2-ccid-device-classes)
3. [PIN Pad Architecture Models](#3-pin-pad-architecture-models)
4. [Remote/Standalone PIN Pads](#4-remotestandalone-pin-pads)
5. [Transaction Confirmation (SWYS)](#5-transaction-confirmation-swys)
6. [CCID Specification Details](#6-ccid-specification-details)
7. [Implementation Status](#7-implementation-status)
8. [Implementation Roadmap](#8-implementation-roadmap)

---

## 1. Executive Summary

### Key Findings

| Question | Answer |
|----------|--------|
| **Is our device a CCID with secure PIN pad?** | **YES** - STM32F469-DISCO with touchscreen is a **CCID Class 4** device (PIN pad with advanced display) |
| **Can PIN pad be standalone from card reader?** | **YES** - CCID/PC/SC supports both integrated and remote PIN pad models |
| **Can PIN pad display transaction for confirmation?** | **YES** - This is "Sign What You See" (SWYS), a standard feature of Class 3/4 readers |

### Current Implementation Gap

| Component | Status | Action Needed |
|-----------|--------|---------------|
| PIN pad modules (state, UI, APDU) | ✅ Complete | None |
| CCID descriptor `bPINSupport` | ❌ Set to `0x00` | Change to `0x01` or `0x03` |
| CCID descriptor `wLcdLayout` | ❌ Set to `0x0000` | Set to touchscreen dimensions |
| `PC_to_RDR_Secure` handler (0x69) | ❌ Returns `CMD_NOT_SUPPORTED` | Full implementation needed |
| `dwFeatures` LCD bit | ❌ Not set | Set bit `0x00040000` |

---

## 2. CCID Device Classes

The CCID specification defines four reader classes based on hardware capabilities:

### Class Hierarchy

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         CCID READER CLASSIFICATION                          │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Class 1: Standard Reader                                                   │
│  ┌─────────────────────┐                                                    │
│  │ ┌─────────────────┐ │                                                    │
│  │ │   Card Slot     │ │  No keypad, no display                            │
│  │ └─────────────────┘ │  PIN entered on PC keyboard (INSECURE)            │
│  │                     │                                                    │
│  └─────────────────────┘                                                    │
│                                                                             │
│  Class 2: PIN Pad Reader                                                    │
│  ┌─────────────────────┐                                                    │
│  │ ┌─────────────────┐ │                                                    │
│  │ │   Card Slot     │ │  Keypad present                                   │
│  │ └─────────────────┘ │  Usually no display (or status LEDs only)         │
│  │ ┌───┐ ┌───┐ ┌───┐  │  PIN entered on device                            │
│  │ │ 1 │ │ 2 │ │ 3 │  │                                                    │
│  │ └───┘ └───┘ └───┘  │                                                    │
│  └─────────────────────┘                                                    │
│                                                                             │
│  Class 3: Display PIN Pad                                                   │
│  ┌─────────────────────┐                                                    │
│  │ ┌─────────────────┐ │  Keypad + Character LCD                           │
│  │ │  LCD Display    │ │  Can show simple text prompts                     │
│  │ │ "Enter PIN"     │ │  Basic transaction confirmation                   │
│  │ └─────────────────┘ │                                                    │
│  │ ┌─────────────────┐ │                                                    │
│  │ │   Card Slot     │ │                                                    │
│  │ └─────────────────┘ │                                                    │
│  │ ┌───┐ ┌───┐ ┌───┐  │                                                    │
│  │ │ 1 │ │ 2 │ │ 3 │  │                                                    │
│  │ └───┘ └───┘ └───┘  │                                                    │
│  └─────────────────────┘                                                    │
│                                                                             │
│  Class 4: Advanced PIN Pad (OUR TARGET)                                     │
│  ┌─────────────────────────────┐                                            │
│  │ ┌─────────────────────────┐ │  Keypad + Graphics Display/Touchscreen    │
│  │ │   Touchscreen Display   │ │  Full transaction confirmation (SWYS)     │
│  │ │  "Pay $125 to Alice?"   │ │  Complex UI flows                         │
│  │ │  [CANCEL]    [CONFIRM]  │ │  Ideal for Bitcoin/cryptocurrency        │
│  │ └─────────────────────────┘ │                                            │
│  │ ┌─────────────────────────┐ │                                            │
│  │ │       Card Slot         │ │                                            │
│  │ └─────────────────────────┘ │                                            │
│  │ ┌───┐ ┌───┐ ┌───┐          │                                            │
│  │ │ 1 │ │ 2 │ │ 3 │          │                                            │
│  │ └───┘ └───┘ └───┘          │                                            │
│  └─────────────────────────────┘                                            │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Descriptor Fields for Class Identification

| Field | Class 1 | Class 2 | Class 3 | Class 4 |
|-------|---------|---------|---------|---------|
| `bPINSupport` | `0x00` | `0x01`+ | `0x01`+ | `0x01`+ |
| `wLcdLayout` | `0x0000` | `0x0000` | `0xLLCC`* | `0xLLCC`* |
| `dwFeatures` LCD bit | Not set | Not set | Set | Set |

*`wLcdLayout` format: High byte = lines, Low byte = characters per line

**For our STM32F469-DISCO touchscreen:**
- `bPINSupport = 0x03` (verify + modify)
- `wLcdLayout = 0x1010` (16 lines × 16 chars placeholder, or use `0x0000` with custom features)
- `dwFeatures |= 0x00040000` (LCD supported)

---

## 3. PIN Pad Architecture Models

### Model A: Integrated PIN Pad (Traditional)

The PIN pad and card reader are in the same physical device:

```
┌─────────────────────────────────────┐
│  Single CCID Device (Class 2/3/4)   │
│                                     │
│  ┌───────────────────────────────┐  │
│  │      PIN Pad (keypad +        │  │
│  │          display)             │  │
│  └───────────────┬───────────────┘  │
│                  │                   │
│                  │ Internal APDU     │
│                  │ Construction      │
│                  │                   │
│  ┌───────────────▼───────────────┐  │
│  │         Card Slot             │  │
│  │    (smart card inserted)      │  │
│  └───────────────────────────────┘  │
│                                     │
└─────────────────────────────────────┘
              │
              │ USB CCID
              ▼
        ┌──────────┐
        │   Host   │
        │   (PC)   │
        └──────────┘
```

**Flow:**
1. Host sends `PC_to_RDR_Secure` with APDU template
2. Reader captures PIN on device
3. Reader constructs VERIFY APDU internally
4. Reader sends APDU to card in local slot
5. Reader returns card response to host

### Model B: Standalone PIN Pad (Remote Card)

The PIN pad is a separate device from the card reader:

```
┌──────────────────┐                   ┌──────────────────┐
│  PIN Pad Device  │                   │  Card Reader     │
│  (CCID Class 3/4)│                   │  (CCID Class 1)  │
│                  │                   │                  │
│  ┌────────────┐  │      USB/Network  │  ┌────────────┐  │
│  │ Touchscreen│  │◄─────────────────►│  │ Card Slot  │  │
│  │ + Keypad   │  │                   │  │            │  │
│  └────────────┘  │                   │  └────────────┘  │
│                  │      Host         │                  │
│  bPINSupport=    │◄─────────────────►│  bPINSupport=    │
│    0x01/0x03     │                   │    0x00          │
└──────────────────┘                   └──────────────────┘
         │                                      │
         │                                      │
         └──────────────┬───────────────────────┘
                        │
                        ▼
                  ┌──────────┐
                  │   Host   │
                  │   (PC)   │
                  └──────────┘
```

**Flow (Host-Mediated):**
1. Host sends `PC_to_RDR_Secure` to PIN pad device
2. PIN pad captures PIN, returns to host (encrypted or as APDU)
3. Host forwards APDU to card reader device
4. Card reader sends to card, returns response to host
5. Host may send confirmation back to PIN pad

**Use Cases:**
- High-security environments where PIN entry is physically separated
- Mobile PIN pads paired with desktop readers
- Multi-reader setups with shared PIN pad

### Model C: Combined Device with Multiple Slots

A single CCID device with multiple slots (e.g., PIN pad slot + card slot):

```
┌─────────────────────────────────────┐
│    Single CCID Device (Multi-Slot)  │
│                                     │
│  ┌───────────────────────────────┐  │
│  │      PIN Pad Interface        │  │
│  │      (Virtual Slot 0)         │  │
│  └───────────────────────────────┘  │
│                                     │
│  ┌───────────────────────────────┐  │
│  │       Card Slot (Slot 1)      │  │
│  └───────────────────────────────┘  │
│                                     │
│  bMaxSlotIndex = 1                  │
└─────────────────────────────────────┘
```

---

## 4. Remote/Standalone PIN Pads

### CCID/PC/SC Support for Remote PIN Entry

The CCID and PC/SC specifications support remote PIN entry through:

1. **Local PIN Capture Mode**: The PIN pad captures PIN and returns it to host
2. **APDU Forwarding**: PIN pad constructs APDU, host forwards to any reader
3. **PC/SC Part 10 Partitions**: Multiple logical slots on single device

### How It Works

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        REMOTE PIN ENTRY FLOW                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Step 1: Host Initiates PIN Entry                                          │
│  ─────────────────────────────────                                         │
│  Host ──PC_to_RDR_Secure──► PIN Pad Device                                 │
│         (APDU template,     (no card in slot)                              │
│          PIN parameters)                                                    │
│                                                                             │
│  Step 2: PIN Pad Captures PIN                                              │
│  ─────────────────────────────────                                         │
│  PIN Pad shows UI, user enters PIN                                         │
│  PIN Pad constructs APDU: [CLA INS P1 P2 Lc PIN_BYTES]                     │
│                                                                             │
│  Step 3: PIN Pad Returns to Host                                           │
│  ─────────────────────────────────                                         │
│  PIN Pad ──RDR_to_PC_DataBlock──► Host                                     │
│           (constructed APDU or                                              │
│            encrypted PIN)                                                   │
│                                                                             │
│  Step 4: Host Forwards to Card Reader                                      │
│  ─────────────────────────────────                                         │
│  Host ──PC_to_RDR_XfrBlock──► Card Reader                                  │
│         (APDU from PIN Pad)  (card inserted)                               │
│                                                                             │
│  Step 5: Card Reader Sends to Card                                         │
│  ─────────────────────────────────                                         │
│  Card Reader ──ISO 7816──► Smart Card                                      │
│               (VERIFY APDU)                                                 │
│                                                                             │
│  Step 6: Response Propagates Back                                          │
│  ─────────────────────────────────                                         │
│  Card ◄──SW 9000──► Card Reader ◄──Host──► Application                    │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Real-World Examples

| Device | Model | Description |
|--------|-------|-------------|
| **Gemalto Ezio Shield Pro** | Standalone | External PIN pad for online banking SWYS |
| **REINER SCT cyberJack** | Integrated/Remote | German eID reader, supports remote scenarios |
| **vSmartCard** | Software | Virtual smart card with network forwarding |
| **SpringCard SpringCore** | Modular | Supports PIN pad as separate module |

---

## 5. Transaction Confirmation (SWYS)

### What is SWYS?

**Sign What You See (SWYS)** or **What You See Is What You Sign (WYSIWYS)** is a security pattern where:

1. The reader displays transaction details on its trusted screen
2. User confirms the transaction on the device
3. Only after confirmation does PIN entry proceed
4. The card signs exactly what the user confirmed

### Why SWYS Matters

| Attack Vector | Without SWYS | With SWYS |
|---------------|--------------|-----------|
| Malicious host software | Can change transaction data after user approves | User sees actual data on trusted display |
| Man-in-the-middle | Can intercept and modify APDU | Display is end-to-end trusted |
| Screen spoofing | PC display can be faked | Reader display cannot be spoofed by PC |

### SWYS Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     SIGN WHAT YOU SEE (SWYS) FLOW                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Phase 1: Transaction Display                                              │
│  ────────────────────────────                                              │
│                                                                             │
│  Host sends transaction data:                                               │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  PC_to_RDR_Secure or PC_to_RDR_DisplayData                          │   │
│  │  ┌─────────────────────────────────────────────────────────────┐   │   │
│  │  │  Transaction:                                                │   │   │
│  │  │    Amount:    $125.00 USD                                    │   │   │
│  │  │    Recipient: Alice Smith                                    │   │   │
│  │  │    Reference: INV-2024-001                                   │   │   │
│  │  │    Timestamp: 2026-03-11 12:30 UTC                           │   │   │
│  │  └─────────────────────────────────────────────────────────────┘   │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Phase 2: User Confirmation                                                │
│  ────────────────────────────                                              │
│                                                                             │
│  Reader displays on trusted screen:                                         │
│  ┌─────────────────────────────────────┐                                   │
│  │     STM32F469-DISCO Touchscreen     │                                   │
│  │  ┌───────────────────────────────┐  │                                   │
│  │  │  Confirm Transaction          │  │                                   │
│  │  │                               │  │                                   │
│  │  │  Amount:  $125.00 USD         │  │                                   │
│  │  │  To:      Alice Smith         │  │                                   │
│  │  │  Ref:     INV-2024-001        │  │                                   │
│  │  │                               │  │                                   │
│  │  │  Is this correct?             │  │                                   │
│  │  └───────────────────────────────┘  │                                   │
│  │                                     │                                   │
│  │    [CANCEL]        [✓ CONFIRM]      │                                   │
│  │                                     │                                   │
│  └─────────────────────────────────────┘                                   │
│                                                                             │
│  User physically presses [CONFIRM] button                                   │
│                                                                             │
│  Phase 3: PIN Entry                                                        │
│  ────────────────────────────                                              │
│                                                                             │
│  After confirmation, reader shows PIN entry:                                │
│  ┌─────────────────────────────────────┐                                   │
│  │     STM32F469-DISCO Touchscreen     │                                   │
│  │  ┌───────────────────────────────┐  │                                   │
│  │  │  Enter PIN                    │  │                                   │
│  │  │                               │  │                                   │
│  │  │  PIN: ****                    │  │                                   │
│  │  └───────────────────────────────┘  │                                   │
│  │                                     │                                   │
│  │  ┌───┐ ┌───┐ ┌───┐                  │                                   │
│  │  │ 1 │ │ 2 │ │ 3 │                  │                                   │
│  │  ├───┤ ├───┤ ├───┤                  │                                   │
│  │  │ 4 │ │ 5 │ │ 6 │                  │                                   │
│  │  ├───┤ ├───┤ ├───┤                  │                                   │
│  │  │ 7 │ │ 8 │ │ 9 │                  │                                   │
│  │  ├───┤ ├───┤ ├───┤                  │                                   │
│  │  │ * │ │ 0 │ │ # │                  │                                   │
│  │  └───┘ └───┘ └───┘                  │                                   │
│  │                                     │                                   │
│  │    [CANCEL]           [OK]          │                                   │
│  └─────────────────────────────────────┘                                   │
│                                                                             │
│  Phase 4: Card Operation                                                   │
│  ────────────────────────────                                              │
│                                                                             │
│  Reader constructs APDU and sends to card:                                  │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  CLA=00 INS=20 P1=00 P2=81 Lc=08 [PIN_BYTES_8]                      │   │
│  │  (VERIFY command with user PIN)                                      │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Card processes and returns SW 9000 (success) or 63Cx (failed)             │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Security Standards Requiring SWYS

| Standard | Domain | SWYS Requirement |
|----------|--------|------------------|
| **PCI PTS 4.x** | Banking | Required for Class 3+ devices |
| **BSI-CC-PP-0083** | eID (German) | Required for "Trusted Channel" |
| **ZKA/Girocard** | German Banking | "Secoder" standard mandates display |
| **ISO 13491** | Secure Cryptographic Devices | Recommended for SCDs |

---

## 6. CCID Specification Details

### bPINSupport Field

Location: CCID Class Descriptor, offset 52 (after wLcdLayout)

| Value | Meaning |
|-------|---------|
| `0x00` | No PIN support (Class 1) |
| `0x01` | PIN Verification supported |
| `0x02` | PIN Modification supported |
| `0x03` | Both Verification and Modification supported |

### wLcdLayout Field

Location: CCID Class Descriptor, offset 50-51

Format: `(lines << 8) | characters_per_line`

| Value | Meaning |
|-------|---------|
| `0x0000` | No LCD |
| `0x0210` | 2 lines × 16 characters |
| `0x0414` | 4 lines × 20 characters |
| Custom | For graphics displays, use placeholder |

### dwFeatures Bits (PIN Pad Related)

| Bit | Hex Value | Meaning |
|-----|-----------|---------|
| 2 | `0x00000004` | Automatic activation of ICC on insert |
| 16 | `0x00010000` | Automatic PIN verification supported |
| 18 | `0x00040000` | LCD supported (for Class 3/4) |
| 23 | `0x00800000` | PIN confirmation supported (SWYS) |

### PC_to_RDR_Secure Command (0x69)

This is the primary command for secure PIN operations.

**Request Format (CCID Rev 1.1 §6.1.11):**

| Offset | Field | Size | Description |
|--------|-------|------|-------------|
| 0 | bMessageType | 1 | `0x69` (PC_to_RDR_Secure) |
| 1-4 | dwLength | 4 | Length of data following header |
| 5 | bSlot | 1 | Slot number (usually 0) |
| 6 | bSeq | 1 | Sequence number |
| 7 | bBWI | 1 | Block waiting timeout |
| 8-9 | wLevelParameter | 2 | Level parameter |
| 10+ | Data | var | PIN Verification/Modification Structure |

**PIN Verification Data Structure:**

| Offset | Field | Size | Description |
|--------|-------|------|-------------|
| 0 | bTimerOut | 1 | Timeout in seconds (0 = no timeout) |
| 1 | bmFormatString | 1 | PIN format (ASCII/Binary, justification) |
| 2 | bmPINBlockString | 1 | PIN block length |
| 3 | bmPINLengthFormat | 1 | PIN length format |
| 4-5 | wPINMaxExtraDigit | 2 | Max (high), Min (low) PIN length |
| 6 | bEntryValidationCondition | 1 | When to validate (OK key, timeout, etc.) |
| 7 | bNumberMessage | 1 | Number of messages to display (0-3) |
| 8-9 | wLangId | 2 | Language ID (e.g., 0x0409 = English) |
| 10 | bMsgIndex | 1 | Message index (0-2) |
| 11 | bTeoPrologue | 1 | TPDU prologue byte |
| 12+ | abPINApdu | var | APDU template (CLA INS P1 P2 [Lc]) |

**Response Format:**

| Offset | Field | Size | Description |
|--------|-------|------|-------------|
| 0 | bMessageType | 1 | `0x80` (RDR_to_PC_DataBlock) |
| 1-4 | dwLength | 4 | Length of response data |
| 5 | bSlot | 1 | Slot number |
| 6 | bSeq | 1 | Sequence number (echoed) |
| 7 | bmStatus | 1 | Command status + ICC status |
| 8 | bError | 1 | Error code (0xEF=cancelled, 0xF0=timeout) |
| 9 | bChainParameter | 1 | Chain parameter |
| 10+ | Data | var | Card response (SW1 SW2 [+ data]) |

### Error Codes for PIN Entry

| Code | Meaning |
|------|---------|
| `0x00` | Success |
| `0xEF` | PIN cancelled by user |
| `0xF0` | PIN entry timeout |
| `0xF3` | Deactivated protocol |
| `0xF6` | ICC protocol not supported |

---

## 7. Implementation Status

### Current Implementation (as of 2026-03-11)

| Component | File | Status | Notes |
|-----------|------|--------|-------|
| **CCID Descriptor** | `src/ccid.rs:137` | ❌ Incomplete | `bPINSupport = 0x00` |
| **Secure Handler** | `src/ccid.rs:415` | ❌ Stub | Returns `CMD_NOT_SUPPORTED` |
| **PIN Buffer** | `src/pinpad/mod.rs` | ✅ Complete | Secure volatile storage |
| **PIN Verify Params** | `src/pinpad/mod.rs` | ✅ Complete | CCID structure parser |
| **State Machine** | `src/pinpad/state.rs` | ✅ Complete | Full state handling |
| **Touchscreen UI** | `src/pinpad/ui.rs` | ✅ Complete | embedded-graphics |
| **APDU Builder** | `src/pinpad/apdu.rs` | ✅ Complete | VERIFY construction |

### Code References

**Current bPINSupport (needs change):**
```rust
// src/ccid.rs:165
0x00, 0x00, // wLcdLayout: 0x0000 (no LCD)
0x00,       // bPINSupport: 0x00 (no PIN support)  <-- CHANGE THIS
0x01,       // bMaxCCIDBusySlots: 1
```

**Current Secure Handler (needs implementation):**
```rust
// src/ccid.rs:415-418
PC_TO_RDR_SECURE => {
    defmt::debug!("CCID: Secure command (stub - no PIN hardware)");
    self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
}
```

---

## 8. Implementation Roadmap

### Phase 1: Enable PIN Support in Descriptor

**Goal:** Advertise PIN pad capabilities to host

**Changes:**
```rust
// src/ccid.rs - CCID_CLASS_DESCRIPTOR_DATA
// Change from:
0x00,       // bPINSupport: 0x00 (no PIN support)

// To:
0x03,       // bPINSupport: 0x03 (verify + modify)

// Also consider updating wLcdLayout for touchscreen:
0x10, 0x10, // wLcdLayout: 16x16 placeholder (or use custom encoding)

// And ensure dwFeatures has LCD bit:
// Current: 0xB2, 0x07, 0x02, 0x00
// Add bit 18 (0x00040000): 0xB2, 0x07, 0x06, 0x00
```

**Verification:**
- `lsusb -v` should show `bPINSupport` = 0x03
- `pcsc_scan` should detect reader as having PIN capabilities

### Phase 2: Implement PC_to_RDR_Secure Handler

**Goal:** Handle incoming PIN verification requests

**Implementation:**
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
    
    // 2. Start PIN entry UI flow
    // (This would integrate with the existing state machine)
    
    // 3. Wait for PIN entry completion
    // (Polling-based or async depending on architecture)
    
    // 4. Construct VERIFY APDU
    // 5. Send to card
    // 6. Return response to host
}
```

### Phase 3: Integrate State Machine with CCID

**Goal:** Connect existing PIN pad modules to CCID handler

**Flow:**
1. `handle_secure()` creates `PinEntryContext`
2. Context drives touchscreen UI updates
3. Touch events feed into state machine
4. On completion, construct APDU and send to card
5. Return card response via CCID DataBlock

### Phase 4: Add SWYS Support

**Goal:** Enable transaction confirmation before PIN entry

**New Features:**
- Parse transaction data from host
- Display confirmation screen
- Wait for physical confirmation
- Only then proceed to PIN entry

**CCID Extension:**
Use `bNumberMessage` and `wLangId` fields to send display text, or implement vendor-specific `PC_to_RDR_DisplayData` command.

---

## References

### Specifications

- **CCID Rev 1.1**: https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf
- **PC/SC Part 10 v2.02.09**: https://pcscworkgroup.com/Download/Specifications/pcsc10_v2.02.09.pdf
- **BSI-CC-PP-0083**: https://www.commoncriteriaportal.org/nfs/ccpfiles/files/ppfiles/pp0083b_pdf.pdf
- **PCI PTS v5.1**: https://www.pcisecuritystandards.org/document_library/

### Commercial References

- **Gemalto IDBridge**: Class 3 banking readers with SWYS
- **REINER SCT cyberJack**: German eID/banking readers
- **ACS ACR83 PINeasy**: Low-cost Class 3 reader
- **MagTek DynaFlex**: Modern PTS 4.x compliant touchscreen reader

### Open Source

- **OpenSC**: Host middleware with PIN pad support
- **osmo-ccid-firmware**: Reference CCID implementation (no PIN pad)
- **vSmartCard**: Virtual smart card with remote forwarding

---

## Changelog

| Date | Author | Changes |
|------|--------|---------|
| 2026-03-11 | AI Research | Initial comprehensive architecture document |
