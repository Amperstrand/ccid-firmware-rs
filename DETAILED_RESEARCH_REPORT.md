# CCID Reader — Detailed Research Report

**Generated:** 2026-03-08  
**Purpose:** Comprehensive research findings with sources, spec details, and implementation guidance for LLM developers.

---

## Executive Summary

This report consolidates exhaustive research on the ccid-reader firmware, focusing on:

1. **PIN OVERLAP INVESTIGATION** — Debug pins vs smartcard pins (CRITICAL)
2. **ATR TRUNCATION ROOT CAUSE** — Why only 1 byte is received
3. **Protocol selection and SCARD_E_PROTO_MISMATCH** 
4. **Implementation comparisons with osmo-ccid-firmware**
5. **Actionable recommendations**

---

## 1. PIN OVERLAP INVESTIGATION (CRITICAL)

### 1.1 Pin Assignments in ccid-reader

| Pin | Function | Mode | Source |
|-----|----------|------|--------|
| **PA2** | USART2_TX (Smartcard IO) | AF7, Open-Drain, Pull-up | `main.rs:122-127` |
| **PA4** | USART2_CK (Smartcard CLK) | AF7, Push-Pull | `main.rs:129-132` |
| **PG10** | Smartcard RST | GPIO Output, Active LOW | `main.rs:134-136` |
| **PC2** | Smartcard PRES | GPIO Input, HIGH=present | `main.rs:138` |
| **PC5** | Smartcard PWR | GPIO Output, LOW=power ON | `main.rs:139-141` |
| **PA11** | USB DM | AF10 | `main.rs:146` |
| **PA12** | USB DP | AF10 | `main.rs:147` |

### 1.2 Debug Pins (SWD) on STM32F469

| Pin | Function | Notes |
|-----|----------|-------|
| **PA13** | SWDIO | Serial Wire Debug Data |
| **PA14** | SWCLK | Serial Wire Debug Clock |

### 1.3 Conflict Analysis

**FINDING: NO PIN CONFLICT EXISTS**

- Smartcard pins (PA2, PA4, PG10, PC2, PC5) are **completely separate** from debug pins (PA13, PA14)
- USB pins (PA11, PA12) are also separate
- The ST-Link on STM32F469-DISCO uses PA13/PA14 exclusively for SWD
- RTT/defmt uses **software buffering only** — no additional pins required

**Evidence from codebase search:**
- `VERIFICATION.md:197-199`: Documents that ST-Link uses PA13/PA14, explicitly states "no pin conflict"
- Pin mapping file (`specter-diy/f469-disco/micropython/ports/stm32/boards/STM32F469DISC/pins.csv`) confirms smartcard pins are separate from SWD

### 1.4 Debug/RTT Configuration

**From `.cargo/config.toml`:**
```toml
[target.thumbv7em-none-eabihf]
runner = "probe-rs run --chip STM32F469NIHx"

[build]
target = "thumbv7em-none-eabihf"
rustflags = ["-C", "link-arg=-Tlink.x", "-C", "link-arg=-Tdefmt.x"]

[env]
DEFMT_LOG = "info"
```

**From `Cargo.toml`:**
```toml
defmt = "0.3"
defmt-rtt = "0.4"
panic-probe = { version = "0.3", features = ["print-defmt"] }
```

**Key finding:** RTT (Real-Time Transfer) uses the **debug mailbox in RAM**, not any GPIO pins. It communicates through the debug interface's memory-mapped registers.

### 1.5 CONCLUSION: Debug Does NOT Cause Pin Conflicts

| Concern | Result | Evidence |
|---------|--------|----------|
| SWD pins overlap smartcard pins? | **NO** | PA13/PA14 ≠ PA2/PA4/PG10/PC2/PC5 |
| RTT uses additional pins? | **NO** | RTT uses debug mailbox in RAM |
| Probe-rs affects GPIO? | **NO** | Only accesses debug core via SWD |

---

## 2. ATR TRUNCATION ROOT CAUSE ANALYSIS

### 2.1 Observed Behavior

From `defmt_capture.log`:
```
[INFO ] ATR TS: 0x3B (ccid_reader ccid-reader/src/smartcard.rs:433)
[INFO ] ATR len=1 hex=[3b] (ccid_reader ccid-reader/src/smartcard.rs:373)
[INFO ] ATR OK, len=1, protocol=T=0 (ccid_reader ccid-reader/src/smartcard.rs:385)
```

**The ATR is truncated to only 1 byte (TS=0x3B). The T0 byte never arrives.**

### 2.2 ATR Reception Code Analysis

**From `smartcard.rs:404-449`:**

```rust
fn read_atr(&mut self) -> Result<(), SmartcardError> {
    // Wait for first byte (TS)
    let mut timeout_ms = SC_ATR_TIMEOUT_MS;  // 400ms
    loop {
        if self.usart.sr().read().rxne().bit_is_set() {
            break;
        }
        Self::delay_ms(1);
        timeout_ms -= 1;
        if timeout_ms == 0 {
            return Err(SmartcardError::Timeout);
        }
    }
    
    // Read TS byte
    let mut ts = self.usart.dr().read().dr().bits() as u8;
    self.atr.raw[0] = ts;
    self.atr.len = 1;
    
    // *** CRITICAL: 20ms delay before reading next byte ***
    Self::delay_ms(20);
    
    // Read subsequent bytes
    for i in 1..SC_ATR_MAX_LEN {
        timeout_ms = SC_ATR_BYTE_TIMEOUT_MS;  // 1000ms per byte
        while !self.usart.sr().read().rxne().bit_is_set() {
            Self::delay_ms(1);
            timeout_ms -= 1;
            if timeout_ms == 0 {
                // *** TIMEOUT: Return with truncated ATR ***
                return Ok(());  // Returns successfully with len=1
            }
        }
        self.atr.raw[i] = self.usart.dr().read().dr().bits() as u8;
        self.atr.len += 1;
    }
}
```

### 2.3 Potential Causes of Truncated ATR

Based on ISO 7816-3 specification and research:

| Cause | Description | Likelihood |
|-------|-------------|------------|
| **1. USART Overrun (ORE)** | If RXNE not cleared fast enough, ORE blocks further reception | **HIGH** |
| **2. Parity Error (PE)** | Card sends parity bit but USART misconfigures it | **MEDIUM** |
| **3. Guard Time Too Short** | 12 ETUs minimum between chars; if violated, framing error | **MEDIUM** |
| **4. Clock/ETU Mismatch** | If clock drifts, T0 byte arrives at wrong time | **MEDIUM** |
| **5. Voltage Too Low** | Card browning out after initial TS transmission | **LOW** |
| **6. Card Defect** | Card only sends TS then stops | **LOW** |

### 2.4 USART Configuration Analysis

**From `smartcard.rs:214-254`:**

```rust
// CR1: TE, RE, M (9 bits), PCE (parity), PS (even)
self.usart.cr1().write(|w| unsafe { w.bits(0x340C) });
// CR2: CLKEN, CPOL, CPHA, LBCL, stop bits
self.usart.cr2().write(|w| unsafe { w.bits(0x3800) });
// CR3: SCEN (smartcard), NACK
self.usart.cr3().write(|w| unsafe { w.bits(0x0030) });
// GTPR: Guard time 16, prescaler 5
self.usart.gtpr().write(|w| unsafe { w.bits((16u16 << 8) | 5) });
```

**CR1 = 0x340C breakdown:**
- Bit 13 (M): 1 = 9-bit data (8 data + 1 parity) ✓
- Bit 10 (PCE): 1 = Parity control enabled ✓
- Bit 9 (PS): 0 = Even parity ✓
- Bit 3 (TE): 1 = Transmitter enabled ✓
- Bit 2 (RE): 1 = Receiver enabled ✓

**CR3 = 0x0030 breakdown:**
- Bit 5 (SCEN): 1 = Smartcard mode enabled ✓
- Bit 4 (NACK): 1 = NACK enabled (should be **DISABLED** for ATR!)

### 2.5 CRITICAL ISSUE: NACK During ATR

**FINDING:** The firmware enables NACK (CR3 bit 4 = 1) during ATR reception.

According to ISO 7816-3:
- **During ATR**, the reader should **NOT** send NACK signals
- NACK is only used during **T=0 protocol** for parity error recovery
- If NACK is enabled during ATR and a parity glitch occurs, the reader sends NACK which confuses the card

**Recommendation:** Disable NACK (CR3 bit 4 = 0) during ATR, enable only after protocol negotiation.

### 2.6 Missing: Error Flag Checking

The code does **NOT** check for:
- **ORE (Overrun Error)** - bit 3 in SR
- **PE (Parity Error)** - bit 2 in SR
- **FE (Framing Error)** - bit 1 in SR
- **NE (Noise Error)** - bit 2 in CR1

**osmo-ccid-firmware comparison:** Uses explicit error handling via `ISO7816_E_RX_ERR_IND` event.

### 2.7 Missing: USART Error Flag Clearing

Before reading ATR, the code drains the RX buffer but does **NOT** clear error flags:

```rust
// Current code only drains data:
while self.usart.sr().read().rxne().bit_is_set() && drain_count < 16 {
    let stale = self.usart.dr().read().dr().bits() as u8;
    drain_count += 1;
}
```

**Should also clear errors:**
```rust
// Clear ORE by reading SR then DR
if self.usart.sr().read().ore().bit_is_set() {
    let _ = self.usart.dr().read().dr().bits();  // Clear ORE
}
```

---

## 3. Protocol Selection and SCARD_E_PROTO_MISMATCH

### 3.1 Error Definition

**Source:** `PCSC/src/PCSC/pcsclite.h.in:137`
```c
#define SCARD_E_PROTO_MISMATCH ((LONG)0x8010000F)
// "The requested protocols are incompatible with the protocol currently in use with the smart card."
```

### 3.2 Where pcscd Generates This Error

**Source:** `PCSC/src/winscard.c`

| Location | Condition | Line |
|----------|-----------|------|
| SCardConnect | dwPreferredProtocols lacks T0/T1/RAW bits | 246 |
| SCardConnect | PHSetProtocol returns SET_PROTOCOL_WRONG_ARGUMENT | 698 |
| SCardConnect | Preferred protocols don't match negotiated cardProtocol | 713 |
| SCardReconnect | Similar checks as SCardConnect | 398, 413, 553 |
| SCardTransmit | Send PCI protocol doesn't match card's protocol | 1557 |

### 3.3 libccid Protocol Selection Flow

**Source:** `CCID/src/ifdhandler.c:731-1217`

```
1. CmdPowerOn → receive ATR
2. ATR_GetDefaultProtocol(atr) → parse TD(i) bytes:
   - If TA2 present → specific mode (locked protocol)
   - First TD found → first offered protocol
   - No TD found → default to T=0
3. IFDHSetProtocolParameters:
   - Check dwProtocols in CCID descriptor
   - If not supported → IFD_ERROR_NOT_SUPPORTED
4. PPS_Exchange (if not auto-PPS):
   - Send: FF PPS0 PPS1 PCK
   - Expect: exact echo
5. SetParameters → PC_to_RDR_SetParameters
6. XfrBlock → APDU exchange
```

### 3.4 ATR_GetDefaultProtocol Implementation

**Source:** `CCID/src/towitoko/atr.c:319-364`

```c
int ATR_GetDefaultProtocol(ATR_t * atr, int *protocol, int *availableProtocols) {
    *protocol = PROTOCOL_UNSET;
    
    // Scan TD(i) bytes for protocols
    for (i=0; i<ATR_MAX_PROTOCOLS; i++) {
        if (atr->ib[i][ATR_INTERFACE_BYTE_TD].present) {
            int T = atr->ib[i][ATR_INTERFACE_BYTE_TD].value & 0x0F;
            if (PROTOCOL_UNSET == *protocol)
                *protocol = T;  // First protocol found
            if (availableProtocols)
                *availableProtocols |= 1 << T;
        }
    }
    
    // Specific mode if TA2 present
    if (atr->ib[1][ATR_INTERFACE_BYTE_TA].present) {
        *protocol = atr->ib[1][ATR_INTERFACE_BYTE_TA].value & 0x0F;
        if (availableProtocols)
            *availableProtocols = 1 << *protocol;
    }
    
    // Default to T=0 if nothing found
    if (PROTOCOL_UNSET == *protocol) {
        *protocol = ATR_PROTOCOL_TYPE_T0;
        if (availableProtocols)
            *availableProtocols = 1 << *protocol;
    }
    
    return ATR_OK;
}
```

### 3.5 Why Our Truncated ATR Causes Protocol Mismatch

**Our firmware behavior:**
1. Receives only TS = 0x3B (ATR len=1)
2. No TD1 byte present
3. `detect_protocol_from_atr()` defaults to T=0
4. Host's `ATR_GetDefaultProtocol()` also defaults to T=0

**BUT:** The actual card may be T=1! Without TD1, we can't know.

**libccid's behavior with truncated ATR:**
- May reject ATR if TCK is missing for T≠0 cards
- May infer wrong protocol
- May fail PPS negotiation
- Returns SCARD_E_PROTO_MISMATCH when SetParameters fails

---

## 4. SetParameters Response Format

### 4.1 CCID Specification Layout

**Response (RDR_to_PC_Parameters, 0x82):**

| Offset | Field | Size | Description |
|--------|-------|------|-------------|
| 0 | bMessageType | 1 | 0x82 |
| 1-4 | dwLength | 4 | 5 (T=0) or 7 (T=1), LSB first |
| 5 | bSlot | 1 | Slot number |
| 6 | bSeq | 1 | Sequence number (echo) |
| 7 | bStatus | 1 | Status byte |
| 8 | bError | 1 | Error code |
| 9 | bProtocolNum | 1 | 0x00 (T=0) or 0x01 (T=1) |
| 10+ | abProtocolData | var | 5 bytes (T=0) or 7 bytes (T=1) |

### 4.2 abProtocolData Layouts

**T=0 (5 bytes):**
| Offset | Field | Description |
|--------|-------|-------------|
| 0 | bmFindexDindex | Fi (bits 7-4), Di (bits 3-0) |
| 1 | bmTCCKST0 | Checksum (bit 0), Convention (bit 1) |
| 2 | bGuardTimeT0 | Extra guard time (0-254, 255=0) |
| 3 | bWaitingIntegerT0 | WI (Waiting Integer) |
| 4 | bClockStop | Clock stop support (0-3) |

**T=1 (7 bytes):**
| Offset | Field | Description |
|--------|-------|-------------|
| 0 | bmFindexDindex | Fi (bits 7-4), Di (bits 3-0) |
| 1 | bmTCCKST1 | Checksum (bit 0), Convention (bit 1) |
| 2 | bGuardTimeT1 | Extra guard time |
| 3 | bWaitingIntegersT1 | BWI (bits 7-4), CWI (bits 3-0) |
| 4 | bClockStop | Clock stop support |
| 5 | bIFSC | Information Field Size Card |
| 6 | bNadValue | Node Address (usually 0x00) |

### 4.3 Common Mistakes (from research)

1. **dwLength mismatch:** Only counts abProtocolData, not bProtocolNum
2. **Missing bProtocolNum:** Byte 9 must contain protocol number
3. **Wrong field order:** bmFindexDindex must be first
4. **Convention bit:** Must match TS byte (0x3B=direct=0, 0x3F=inverse=1)

---

## 5. ATR Timing Requirements (ISO 7816-3)

### 5.1 Initial Character Delay

| Parameter | Value | Source |
|-----------|-------|--------|
| Initial Waiting Time (IWT) | 9600 ETU max | ISO 7816-3 §8.1 |
| First byte (TS) arrival | Within 400-9600 ETU after RST released | ISO 7816-3 |
| ETU at reset | 372 clock cycles (default Fi/Di = 372/1) | ISO 7816-3 Table 7/8 |

### 5.2 Inter-Byte Timing

| Parameter | Value | Notes |
|-----------|-------|-------|
| Guard Time (GT) | 12 ETU minimum | Between consecutive characters |
| Extra Guard Time (N) | From TC1 byte in ATR | 0-254 ETU, 255 means 0 |
| Character Frame | 10 ETU minimum (1 start + 8 data + 1 parity) | Plus 2 stop bits |

### 5.3 Our Current Delays

```rust
const SC_POWER_ON_DELAY_MS: u32 = 20;   // After PWR assertion
const SC_RESET_DELAY_MS: u32 = 25;       // RST low duration
const SC_ATR_POST_RST_DELAY_MS: u32 = 5; // After RST high, before ATR read
const SC_ATR_TIMEOUT_MS: u32 = 400;      // For first byte
const SC_ATR_BYTE_TIMEOUT_MS: u32 = 1000; // Per subsequent byte
```

**At 3.5712 MHz clock, 1 ETU ≈ 104 µs:**
- 9600 ETU ≈ 998 ms (max initial delay)
- 12 ETU ≈ 1.25 ms (guard time)

**Our 20 ms delay after TS is adequate** for guard time, but may be too long if the card is ready sooner.

---

## 6. Implementation Guidance for LLM

### 6.1 CRITICAL: Fix ATR Reception

**Problem:** ATR truncated to 1 byte (TS only)

**Root Causes (in priority order):**

1. **NACK enabled during ATR** (CR3 bit 4 = 1)
   - **Fix:** Clear NACK bit before ATR, set after protocol established
   - **Code location:** `smartcard.rs:235` and add new function for ATR-specific config

2. **No error flag checking**
   - **Fix:** Check and clear ORE/PE/FE flags before each byte read
   - **Code location:** `smartcard.rs:404-449` in `read_atr()`

3. **Missing overrun recovery**
   - **Fix:** If ORE detected, read DR to clear, then continue
   - **Reference:** STM32F4 Reference Manual, USART chapter

### 6.2 Implementation Pattern from osmo-ccid-firmware

**From `osmo-ccid-firmware/ccid_common/iso7816_fsm.c`:**

```c
// ATR FSM states (lines 83-91)
enum iso7816_3_state {
    ISO7816_S_RESET,      // In reset
    ISO7816_S_WAIT_ATR,   // Waiting for ATR to start
    ISO7816_S_IN_ATR,     // Receiving ATR
    ISO7816_S_WAIT_TPDU,  // Waiting for TPDU
    ...
};

// ATR sub-states (lines 96-100)
enum atr_state {
    ATR_S_WAIT_TS,   // Initial byte
    ATR_S_WAIT_T0,   // Format byte
    ATR_S_WAIT_TA,   // Interface bytes
    ...
};
```

**Key pattern:** osmo uses a **state machine** with explicit error handling via `ISO7816_E_RX_ERR_IND` event.

### 6.3 Recommended Code Changes

**In `smartcard.rs`, add before `read_atr()`:**

```rust
/// Configure USART for ATR reception (disable NACK)
fn configure_usart_for_atr(&mut self) {
    // Disable NACK during ATR (CR3 bit 4 = 0)
    self.usart.cr3().modify(|_, w| w.nack().clear_bit());
}

/// Configure USART for T=0 protocol (enable NACK)
fn configure_usart_for_t0(&mut self) {
    // Enable NACK for T=0 parity recovery (CR3 bit 4 = 1)
    self.usart.cr3().modify(|_, w| w.nack().set_bit());
}

/// Clear USART error flags
fn clear_usart_errors(&mut self) {
    // Read SR then DR to clear ORE
    let sr = self.usart.sr().read();
    if sr.ore().bit_is_set() {
        let _ = self.usart.dr().read().dr().bits();
    }
}
```

**In `read_atr()`, add error checking:**

```rust
fn read_atr(&mut self) -> Result<(), SmartcardError> {
    // Disable NACK for ATR
    self.configure_usart_for_atr();
    
    // Clear any pending errors
    self.clear_usart_errors();
    
    // ... existing TS read code ...
    
    // Before reading each byte:
    self.clear_usart_errors();
    
    // ... existing byte read loop ...
    
    // After ATR complete, re-enable NACK for T=0 if needed
    if self.protocol == 0 {
        self.configure_usart_for_t0();
    }
    
    Ok(())
}
```

### 6.4 Debugging Steps

1. **Enable LSP diagnostics:** Run `lsp_diagnostics` on `smartcard.rs` before changes
2. **Add detailed logging:** Log SR register value before each byte read
3. **Test with known-good card:** Use a card that works in Gemalto reader
4. **Compare ATR:** Capture ATR from Gemalto vs our reader for same card

---

## 7. Summary of Findings

| Topic | Finding | Source |
|-------|---------|--------|
| **Pin conflicts** | **NONE** - SWD (PA13/PA14) separate from smartcard (PA2/PA4/PG10/PC2/PC5) | Code analysis, VERIFICATION.md |
| **ATR truncation cause** | **NACK enabled during ATR** + missing error flag handling | USART CR3=0x0030 analysis, ISO 7816-3 |
| **Protocol mismatch cause** | Truncated ATR → wrong protocol detection → PPS/SetParameters fail | PCSC winscard.c analysis |
| **SetParameters format** | Our implementation is CORRECT | CCID spec, osmo-ccid-firmware |
| **Debug impact** | RTT uses RAM mailbox, no pin conflicts | probe-rs documentation |

---

## 8. Action Items (Priority Order)

1. **CRITICAL:** Disable NACK during ATR reception (CR3 bit 4)
2. **HIGH:** Add USART error flag checking and clearing in `read_atr()`
3. **MEDIUM:** Add logging of SR register during ATR for debugging
4. **LOW:** Consider state-machine approach like osmo for robustness

---

## 9. References

### Local Files
- `ccid-reader/src/smartcard.rs` — USART config, ATR reception
- `ccid-reader/src/main.rs` — Pin assignments
- `ccid-reader/VERIFICATION.md` — Debug pin documentation
- `osmo-ccid-firmware/ccid_common/iso7816_fsm.c` — ATR FSM reference
- `CCID/src/towitoko/atr.c` — ATR_GetDefaultProtocol implementation
- `PCSC/src/winscard.c` — SCARD_E_PROTO_MISMATCH generation

### Specifications
- USB CCID Rev 1.1 — Message formats, SetParameters layout
- ISO 7816-3:2006 — ATR timing, T=0/T=1 protocols
- STM32F4 Reference Manual (RM0090) — USART smartcard mode

### Online Resources
- [ATR Parser](https://smartcard-atr.apdu.fr/) — Validate ATR bytes
- [CCID driver](https://ccid.apdu.fr/) — libccid documentation
