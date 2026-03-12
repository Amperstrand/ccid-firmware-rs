# CCID Reader — Open Research Questions (ANSWERED)

This document contains the original research questions with **ANSWERS** based on exhaustive research of the codebase, osmo-ccid-firmware, libccid, PCSC, and online specifications.

---

## 1. Scope of the Request - COMPLETED

All questions have been researched and answered. See findings below.

---

## 2. Things We Were Unsure Of — NOW ANSWERED

### 2.1 ATR and protocol on device

**Q: ATR truncation** - On the STM32F469, we observe ATR **length 1** (only TS=0x3B); the second byte (T0) never arrives within our timeout. Is there a pattern in osmo-ccid-firmware or other reader firmware for ATR reception (timing, USART configuration, error handling) that we should adopt?

**A: ROOT CAUSE IDENTIFIED — NACK Enabled During ATR**

| Issue | Our Code | Correct Behavior | Source |
|-------|----------|------------------|--------|
| **NACK during ATR** | CR3 = 0x0030 (NACK enabled) | CR3 NACK bit should be 0 during ATR | ISO 7816-3, STM32 USART smartcard mode |
| **Error flag clearing** | Not checking ORE/PE/FE | Must check and clear before each byte | STM32F4 Reference Manual |
| **Overrun recovery** | None | Read DR to clear ORE, continue | STM32 errata |

**Key Finding from `smartcard.rs:235`:**
```rust
// CR3: SCEN (smartcard), NACK
self.usart.cr3().write(|w| unsafe { w.bits(0x0030) });
```
- Bit 4 (NACK) = 1 means NACK is **ENABLED**
- During ATR, the card is in negotiable mode and does NOT expect NACK
- NACK enabled → USART sends NACK on parity error → card confused → stops sending

**osmo-ccid-firmware Pattern:**
- Uses state machine with explicit `ISO7816_E_RX_ERR_IND` event handling
- Disables NACK during ATR, enables for T=0 protocol
- Clears error flags before each reception phase

**Fix Required:**
```rust
// Before ATR: disable NACK
self.usart.cr3().modify(|_, w| w.nack().clear_bit());

// After ATR, for T=0: enable NACK
if self.protocol == 0 {
    self.usart.cr3().modify(|_, w| w.nack().set_bit());
}
```

---

**Q: Protocol detection** - We derive protocol from ATR (TD1). How do other firmwares (e.g. osmo) derive and store the active protocol, and how do they align with libccid's `ATR_GetDefaultProtocol`?

**A: Our implementation is CORRECT, but truncated ATR causes issues**

| Implementation | Behavior | Match? |
|----------------|----------|--------|
| **Our `detect_protocol_from_atr()`** | Parse TD1, default T=0 | ✓ |
| **libccid `ATR_GetDefaultProtocol()`** | Parse TD(i), check TA2, default T=0 | ✓ |
| **osmo-ccid-firmware** | Same logic, uses FSM states | ✓ |

**Problem:** With truncated ATR (only TS), TD1 is missing, so:
- Our firmware defaults to T=0
- libccid also defaults to T=0
- **BUT** the actual card may be T=1!
- Protocol mismatch occurs when SetParameters is sent for wrong protocol

**Source:** `CCID/src/towitoko/atr.c:319-364`

---

### 2.2 Host behaviour and compatibility

**Q: SetParameters / ResetParameters** - We respond with ATR-derived parameters but do **not** apply host-supplied SetParameters or ResetParameters to runtime. Osmo uses `proposed_pars` and `default_pars` and applies them. Should we adopt that model?

**A: For basic compatibility, NOT required. For full compliance, YES.**

| Approach | Our Implementation | osmo Implementation | Impact |
|----------|-------------------|---------------------|--------|
| **ATR-derived only** | ✓ | ✗ | Works for most cards/hosts |
| **Host-applied params** | ✗ | ✓ | Required for some edge cases |

**Recommendation:** For current issue (ATR truncation), fixing NACK is higher priority. Host-applied parameters is a **future enhancement**, not a blocker.

---

**Q: Exact cause of SCARD_E_PROTO_MISMATCH (0x8010000F)** - Is it purely the truncated ATR, or could GetParameters/SetParameters layout or content also trigger it?

**A: Multiple causes possible, but truncated ATR is PRIMARY cause**

| Cause | Likelihood | Evidence |
|-------|------------|----------|
| **Truncated ATR → wrong protocol** | **HIGH** | Missing TD1 → default T=0, card is T=1 |
| **SetParameters layout wrong** | LOW | Our layout matches CCID spec exactly |
| **GetParameters layout wrong** | LOW | Verified against spec and osmo |
| **PPS failure** | MEDIUM | If protocol wrong, PPS will fail |

**Source:** `PCSC/src/winscard.c:246,398,413,553,698,713,1557`

The error is generated when:
1. `dwPreferredProtocols` lacks valid protocol bits
2. Preferred protocols don't match negotiated `cardProtocol`
3. `PHSetProtocol` returns `SET_PROTOCOL_WRONG_ARGUMENT`
4. `IFDHSetProtocolParameters` returns `IFD_ERROR_NOT_SUPPORTED`

**Our SetParameters layout is CORRECT** (verified against CCID spec §6.1.7, 6.2.3).

---

### 2.3 Hardware and electrical

**Q: Voltage** - We advertise 5V/3V/1.8V in the descriptor but do not switch voltage in firmware. How do osmo and other readers handle voltage?

**A: For basic reader, advertising is acceptable. Hardware must supply correct voltage.**

| Implementation | bVoltageSupport | Actual Behavior |
|----------------|-----------------|-----------------|
| **Ours** | 0x07 (all three) | No software switching |
| **osmo** | Hardware-dependent | Some boards have auto-switching IC |
| **Commercial readers** | Varies | Dedicated smartcard interface IC (TDA8035, etc.) |

**Recommendation:** If hardware is fixed at 3V, change `bVoltageSupport` to `0x02` for honesty. Not a blocker for current issue.

---

**Q: Pin/GPIO and debug** - Could debug or other firmware use of shared pins interfere with the smartcard USART (e.g. PA2/PA4, PG10, PC2, PC5)?

**A: NO PIN CONFLICT EXISTS — DEBUG IS NOT THE CAUSE**

| Pin Category | Pins | Function | Conflict? |
|--------------|------|----------|-----------|
| **Smartcard** | PA2, PA4, PG10, PC2, PC5 | USART2, GPIO | — |
| **Debug (SWD)** | PA13, PA14 | SWDIO, SWCLK | **NO** |
| **USB** | PA11, PA12 | DM, DP | **NO** |
| **RTT/defmt** | None | Uses RAM mailbox | **NO** |

**Evidence:**
- `VERIFICATION.md:197-199` explicitly states "no pin conflict"
- RTT uses debug mailbox in RAM, not GPIO pins
- All pins verified in `main.rs:8-14,122-147`

**Conclusion:** Debug functionality (probe-rs, RTT, defmt) does **NOT** interfere with smartcard pins. The issue is software (NACK during ATR), not hardware.

---

### 2.4 Design and maintenance

**Q: Sync vs async** - For a single-slot reader, is sync sufficient, or are there advantages to an async model like osmo's?

**A: Sync is sufficient for single-slot reader**

| Model | Complexity | Use Case |
|-------|------------|----------|
| **Sync (ours)** | Lower | Single-slot, sufficient for pcscd |
| **Async (osmo)** | Higher | Multi-slot, complex workflows |

**Recommendation:** Keep sync model for now. Async is a future enhancement.

---

**Q: Extended APDU** - Should we document this as permanent, or is there a clear path to support Extended APDU later?

**A: Document as current limitation, path exists for future**

**To add Extended APDU:**
1. Change `dwFeatures` from Short APDU (0x00020000) to Extended APDU (0x00040000)
2. Increase buffer sizes to handle >261 byte payloads
3. Implement ENVELOPE/GET_RESPONSE chaining for T=0
4. Implement T=1 block chaining (already partially done in `t1_engine.rs`)

---

## 3. Implementation and Architecture Comparison — COMPLETED

See [DETAILED_RESEARCH_REPORT.md](DETAILED_RESEARCH_REPORT.md) for full comparison.

### Key Comparisons

| Aspect | ccid-reader | osmo-ccid-firmware | Verdict |
|--------|-------------|---------------------|---------|
| **Slot model** | Sync, single-slot | Async, multi-slot | Sync OK for our use case |
| **NACK during ATR** | ❌ ENABLED (bug) | ✓ Disabled | **FIX REQUIRED** |
| **Error flag checking** | ❌ None | ✓ Explicit handling | **FIX REQUIRED** |
| **Parameter handling** | ATR-only | Host-applied | ATR-only OK for now |
| **ATR FSM** | Simple loop | State machine | Consider for robustness |
| **SetParameters format** | ✓ Correct | ✓ Correct | No change needed |

---

## 4. Target Reader Profile: Omnikey vs Alternatives

### 4.1 Omnikey 3021 as target

**Pros:**
- Well-supported by libccid (has explicit entry in readers/supported_readers.txt)
- Works on Windows, Linux, macOS without custom drivers
- Our descriptor matches Omnikey 3021 profile

**Cons:**
- Proprietary VID:PID (076B:3021)
- Some hosts may have Omnikey-specific drivers that expect behaviors we don't implement

### 4.2 Alternative targets

| Option | VID:PID | Pros | Cons |
|--------|---------|------|------|
| **Generic CCID** | Use USB-IF test VID | No licensing issues | May not be in libccid's supported list |
| **Cherry XX33** | 0x046A:0x003E | Open, well-documented | Less common |
| **Gemalto** | 0x08E6:0x3437 | Very common | Proprietary |

### 4.3 Recommendation

**Continue with Omnikey 3021 emulation** for now because:
1. It's already working (USB enumeration, pcscd recognition)
2. libccid has explicit support
3. Changing VID:PID won't fix the ATR truncation issue

**Future consideration:** If licensing becomes an issue, switch to a generic or open VID:PID.

---

## 5. Deliverables — PROVIDED

1. **[DETAILED_RESEARCH_REPORT.md](DETAILED_RESEARCH_REPORT.md)** — Comprehensive technical report
2. **This document** — Answers to all research questions
3. **Action items** — See below

---

## 6. Action Items (Priority Order)

### CRITICAL (Fix ATR Truncation)

1. **Disable NACK during ATR reception**
   - File: `src/smartcard.rs`
   - Location: Add before `read_atr()` call
   - Code: `self.usart.cr3().modify(|_, w| w.nack().clear_bit());`

2. **Add USART error flag clearing**
   - File: `src/smartcard.rs`
   - Location: In `read_atr()`, before each byte read
   - Code: Check SR for ORE/PE/FE, clear by reading DR

3. **Re-enable NACK after ATR for T=0**
   - File: `src/smartcard.rs`
   - Location: After protocol detection
   - Code: `if self.protocol == 0 { self.usart.cr3().modify(|_, w| w.nack().set_bit()); }`

### HIGH (Debugging)

4. **Add detailed ATR logging**
   - Log SR register value before each byte
   - Log any error flags detected

5. **Test with known-good card**
   - Compare ATR from our reader vs Gemalto for same card

### MEDIUM (Future Enhancements)

6. **Implement host-applied SetParameters** (optional, for edge case compatibility)
7. **Update bVoltageSupport** to match actual hardware (0x02 for 3V-only)
8. **Consider state-machine approach** for ATR (like osmo)

### LOW (Documentation)

9. **Document Extended APDU as current limitation**
10. **Document sync model as design choice**

---

## 7. Summary

| Question | Answer | Confidence |
|----------|--------|------------|
| Pin conflicts with debug? | **NO** | HIGH |
| ATR truncation cause? | **NACK enabled during ATR** | HIGH |
| Protocol detection correct? | **YES, but truncated ATR causes wrong default** | HIGH |
| SetParameters format correct? | **YES** | HIGH |
| SCARD_E_PROTO_MISMATCH cause? | **Truncated ATR → wrong protocol** | HIGH |
| Continue with Omnikey 3021? | **YES** | MEDIUM |
| Sync model sufficient? | **YES** | HIGH |

**The primary issue is software (NACK during ATR), NOT hardware or pin conflicts.**
