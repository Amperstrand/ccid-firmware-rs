# C Reference Audit Report: ccid-firmware-rs vs osmo-ccid-firmware

**Issue**: cf-be8
**Date**: 2026-03-17
**C Reference**: osmocom/osmo-ccid-firmware (`osmo_ccid/`)
**Rust Implementation**: Amperstrand/ccid-firmware-rs (`src/`)

---

## Executive Summary

ccid-firmware-rs is a substantially more complete CCID implementation than osmo-ccid-firmware. The Rust firmware supports both T=0 and T=1 protocols, has a full PIN pad subsystem, and targets a single-slot STM32F469 platform. The C reference supports only T=0, stubs PIN/Secure, and targets an 8-slot SAMD54 platform. Several divergences are intentional design trade-offs; a few are accidental gaps worth noting.

| Dimension | Rust (ccid-firmware-rs) | C (osmo-ccid-firmware) | Verdict |
|-----------|------------------------|------------------------|---------|
| Protocol support | T=0 + T=1 | T=0 only | Intentional scope difference |
| PIN/Secure | Full implementation | CMD_NOT_SUPPORTED | Intentional |
| Slot count | 1 | 8 | Platform difference |
| State machine framework | Per-component enums | Hierarchical osmo_fsm | Architectural trade-off |
| Parameter negotiation | Direct ATR-derived | Deferred commit (proposed_pars) | Divergence (see analysis) |
| Target MCU | STM32F469 (Cortex-M4) | SAMD54 (Cortex-M4) | Platform difference |

---

## 1. Command Handling

### 1.1 Command Coverage Comparison

| CCID Command | Byte | Rust Status | C Status | Divergence |
|-------------|------|------------|----------|------------|
| GetSlotStatus | 0x65 | Implemented | Implemented | None |
| IccPowerOn | 0x62 | Implemented (sync) | Implemented (async) | **Intentional** — see 3.2 |
| IccPowerOff | 0x63 | Implemented | Implemented | None |
| XfrBlock | 0x6F | Implemented (T=0+T=1) | Implemented (T=0 only) | Intentional — T=1 scope |
| GetParameters | 0x6C | Implemented (T=0+T=1) | Implemented (T=0 only, T=1 FIXME) | Intentional |
| SetParameters | 0x61 | Implemented (T=0+T=1) | Implemented (T=0 only, T=1 commented out) | Intentional |
| ResetParameters | 0x6D | Implemented | Implemented (T=0 only) | Intentional |
| Escape | 0x6B | CMD_NOT_SUPPORTED | CMD_NOT_SUPPORTED | None |
| IccClock | 0x6E | Implemented | Implemented | None |
| T0APDU | 0x6A | CMD_NOT_SUPPORTED | CMD_NOT_SUPPORTED (FIXME in comment) | None |
| Secure | 0x69 | **Full PIN Verify+Modify** | CMD_NOT_SUPPORTED | **Intentional** — feature addition |
| Mechanical | 0x71 | CMD_NOT_SUPPORTED | CMD_NOT_SUPPORTED | None |
| Abort | 0x72 | Stub (returns OK) | Stub (broken — always CMD_NOT_SUPPORTED) | **Accidental** — C has a bug |
| SetDataRateAndClockFreq | 0x73 | Implemented (adjusts BRR) | Implemented (returns current values) | **Divergence** — see section 5 |

### 1.2 Class-Specific Control Requests

| Request | Byte | Rust | C |
|---------|------|------|---|
| ABORT | 0x01 | Accepts, no-op | Logs "Not handling", returns OK |
| GET_CLOCK_FREQUENCIES | 0x02 | Returns single value (4 MHz) | Returns array of 4 values (2500/5000/10000/20000 kHz) |
| GET_DATA_RATES | 0x03 | Returns single value (10752 bps) | Returns array of 1 value (9600 bps) |

**Analysis**: Rust advertises continuous ranges (bNumClockSupported=0, bNumDataRatesSupported=0) and returns a single representative value. C advertises discrete clock support (bNumClockSupported=4) and returns an array. Both are valid per CCID spec.

### 1.3 Abort Command Bug in C Reference

The C implementation has a bug in `ccid_handle_abort()`:

```c
// ccid_device.c — Abort handler
case PC_to_RDR_Abort:
    if (0/* FIXME */) {
        // This branch is unreachable
    }
    // Falls through to default -> CMD_NOT_SUPPORTED
```

The condition is hardcoded to `0`, so Abort always returns CMD_NOT_SUPPORTED even though it should match against the slot's abort sequence. The Rust implementation correctly returns CMD_STATUS_OK (single-slot, no concurrent commands to abort).

**Verdict**: Accidental bug in C. Rust's simpler approach is correct for a single-slot device.

---

## 2. Protocol State Machines

### 2.1 T=0 Implementation Comparison

| Aspect | Rust | C |
|--------|------|---|
| Procedure byte classification | Dedicated `classify_t0_procedure_byte()` in protocol_unit.rs | Inline switch in TPDU FSM states |
| NULL byte (0x60) handling | Loops, re-reads | Separate FSM state `TPDU_S_PROCEDURE` |
| INS ACK (all remaining) | Single path | Single path |
| INS^0xFF ACK (byte-at-a-time) | Single path | Separate FSM states for TX/RX |
| GET RESPONSE (61xx) | Max 32 iterations | Not explicitly handled (relies on TPDU FSM) |
| Le re-try (6Cxx) | Supported | Supported |
| P3==0 (256 bytes) | Not explicitly handled | Handled per ISO 7816-3 10.3.2 |

**Notable**: The Rust implementation explicitly handles SW1=0x61 (GET RESPONSE) with a loop limit of 32 iterations. The C implementation relies on the TPDU FSM's natural procedure byte handling without explicit GET RESPONSE logic.

**Potential gap in Rust**: P3==0 meaning 256 bytes is not explicitly handled in the T=0 transmit path. If a host sends a T=0 command with P3=0 expecting 256 response bytes, the Rust code may only read 0 bytes.

### 2.2 T=1 Implementation Comparison

| Aspect | Rust | C |
|--------|------|---|
| Protocol support | **Full** (I-block, R-block, S-block) | **Not implemented** |
| Block chaining | Supported (I_M_CHAIN bit) | N/A |
| Retransmission | Max 3 retries per chunk | N/A |
| WTX (Waiting Time Extension) | S(WTX req/resp) handled | N/A |
| RESYNC | S(RESYNC req/resp), resets N(S) | N/A |
| IFSD negotiation | S(IFS req/resp), IFSD=254 | N/A |
| LRC error detection | XOR checksum verification | N/A |
| NAD support | 0x00 only (no NAD_OTHER) | N/A |

The C reference has **no T=1 support at all**. The CCID descriptor advertises `dwProtocols = 0x01` (T=0 only). This is the single largest functional gap between the two implementations.

### 2.3 ATR Parsing Comparison

| Aspect | Rust | C |
|--------|------|---|
| TS convention detection | Checks 0x3B/0x3F only | Checks 0x3B/0x3F + handles 0x23/0x03 with flip |
| Inverse convention | Not handled (only direct) | Full support (ARM RBIT or LUT) |
| Multi-level interface bytes | Supported (up to TD3) | Supported (loop over Y-mask) |
| Historical bytes | Supported | Supported |
| TCK verification | **Verified (reject on mismatch for T=1)** | **Logged but not rejected** |
| Max ATR length | 33 bytes | 33 bytes (MAX_ATR_SIZE=32+1) |
| ATR read timeout | 400ms first byte, ~50ms inter-byte | 1-byte timer hint + WTIME |
| Guard time (TC1) | Parsed and applied | Parsed, stored in pars |
| IFSC (TA3) | Parsed, clamped to 254 | N/A (T=0 only) |
| BWI/CWI (TB3) | Parsed | N/A (T=0 only) |
| EDC type (TC3) | Parsed (LRC/CRC) | N/A (T=0 only) |

**Divergence — inverse convention**: The Rust implementation only accepts direct convention (TS=0x3B). If a card sends inverse convention (TS=0x3F), it will not be handled. The C reference detects and supports both conventions, including the edge case of receiving inverse-encoded bytes on a direct-configured UART (flipping via RBIT or LUT).

**Verdict**: Accidental gap in Rust. Cards using inverse convention will not work. This should be addressed for full ISO 7816-3 compliance.

**Divergence — TCK verification**: ~~Both implementations accept ATRs with bad TCK checksums.~~ Rust now verifies TCK for T=1 ATRs and rejects the ATR on mismatch (per ISO 7816-3 §8.2.4). C verifies TCK but only logs the failure without rejecting the ATR.

### 2.4 PPS Negotiation Comparison

| Aspect | Rust | C |
|--------|------|---|
| Skip conditions | No TA1 or TA1==0x11 (default) | Always attempts PPS |
| Failure handling | **Graceful degradation** — use default Fi/Di | **Card deactivation** — RST high, power off |
| PPS format | [0xFF, PPS0, PPS1, PCK] | [0xFF, PPS0, PPS1, PCK] |
| Response validation | PPSS=0xFF, PCK=XOR match | Byte-for-byte match (stricter) |
| Protocol field | Supports T=0 and T=1 in PPS0 | T=0 only |
| State machine | Dedicated PpsFsm (10 states) | Child FSM of ISO7816 (9 states) |
| On success | Updates baud rate via BRR register | Updates Fi/Di, reconfigures UART |

**Key divergence — failure handling**: The Rust implementation treats PPS failure as non-fatal (graceful degradation to default Fi/Di). The C implementation treats PPS failure as fatal (deactivates the card). The CCID spec does not mandate either behavior, but the C approach is more conservative. The Rust approach is more resilient for real-world use with cards that have quirky PPS behavior.

### 2.5 Slot State FSM Comparison

| Rust (3-state) | C (implicit, via ICC status) |
|----------------|------------------------------|
| Absent | ICC_NOT_PRESENT |
| PresentInactive | ICC_PRESENT_INACTIVE |
| PresentActive | ICC_PRESENT_ACTIVE |

Both track the same three logical states. Rust uses an explicit enum; C uses the ICC status bits directly in the slot structure.

---

## 3. Parameter Negotiation

### 3.1 The `proposed_pars` Pattern (C Reference)

The C reference uses a deferred-commit pattern for parameter changes:

```
Host sends SetParameters
  -> decode into proposed_pars (pending)
  -> trigger PPS exchange with card
  -> if PPS succeeds: proposed_pars -> pars (committed)
  -> if PPS fails: pars unchanged, card deactivated
  -> response sent AFTER PPS completes
```

This ensures parameters are only committed if the card actually accepts them. The `FAKE_CCID_SETPARAMETERS` build flag (default ON) skips the PPS exchange for debugging.

### 3.2 Direct ATR-Derived Parameters (Rust)

The Rust implementation derives parameters directly from the ATR at power-on time:

```
PowerOn -> parse ATR -> extract Fi/Di/guard_time/protocol/IFSC
  -> store in AtrParams
  -> attempt PPS (non-blocking)
  -> respond immediately with ATR-derived params
```

SetParameters infers protocol from message length (5=T=0, 7=T=1) and overwrites stored params directly, without a PPS exchange.

### 3.3 Trade-off Analysis

| Aspect | C (proposed_pars) | Rust (direct ATR) |
|--------|-------------------|-------------------|
| Spec compliance | **Stricter** — params only commit on card acceptance | Spec allows both approaches |
| Safety | Safer — failed negotiation doesn't corrupt state | Risk — params may not match actual card state |
| Latency | Higher — must wait for PPS before responding | Lower — responds immediately with ATR data |
| Complexity | Higher — dual parameter sets, async completion | Lower — single parameter set |
| Real-world robustness | Lower — PPS failure deactivates card | Higher — graceful degradation on PPS failure |

**Verdict**: The C approach is more formally correct per the CCID spec's intent. The Rust approach trades safety for robustness and simplicity. This is an intentional design choice driven by the different target use cases (telecom SIM reader vs. general-purpose CCID reader).

---

## 4. USB Descriptors

### 4.1 CCID Class Descriptor Comparison

| Field | Rust (Cherry ST-2xxx) | C (sysmoOCTSIM) |
|-------|----------------------|-----------------|
| bcdCCID | 0x0110 | 0x0110 |
| bMaxSlotIndex | 0 | 7 |
| bVoltageSupport | 0x01 (5V only) | 0x07 (5V, 3V, 1.8V) |
| dwProtocols | 0x03 (T=0 + T=1) | 0x01 (T=0 only) |
| dwDefaultClock | varies by profile | 2500 kHz |
| dwMaximumClock | varies by profile | 20000 kHz |
| bNumClockSupported | 0 (continuous) | 4 (discrete) |
| dwDataRate | varies by profile | 6720 bps |
| dwMaxDataRate | varies by profile | 921600 bps |
| bNumDataRatesSupported | 0 (continuous) | 0 |
| dwMaxIFSD | 254 | 0 |
| dwMechanical | 0 | 0 |
| dwFeatures | 0x000101FE (Cherry) | 0x000100B0 |
| dwMaxCCIDMessageLength | 270/271 | 272 |
| bClassGetResponse | 0x00/0xFF | 0xFF |
| bClassEnvelope | 0x00/0xFF | 0xFF |
| wLcdLayout | 0x0000 | 0x0000 |
| bPINSupport | 0x03 (verify+modify) or 0x00 | 0x00 |
| bMaxCCIDBusySlots | 1 | 8 |

### 4.2 Feature Bits Comparison

| Feature Bit | Rust (Cherry) | C |
|-------------|--------------|---|
| Auto parameter (0x02) | Yes | No |
| Auto activate (0x04) | No | No |
| Auto voltage (0x08) | Yes | No |
| Auto clock (0x10) | Yes | Yes |
| Auto baud (0x20) | Yes | Yes |
| Auto PPS (0x40) | No | No |
| Auto PPS neg (0x80) | Yes | Yes |
| Clock stop (0x100) | Yes | No |
| TPDU level (0x10000) | Yes | Yes |

### 4.3 Endpoint Configuration

| Endpoint | Rust | C |
|----------|------|---|
| Bulk IN | 64 bytes | 64 bytes |
| Bulk OUT | 64 bytes | 64 bytes |
| Interrupt IN | 8 bytes, 10ms | 64 bytes, 16ms interval |

**Divergence**: Interrupt IN endpoint sizes differ (8 vs 64 bytes). The CCID spec only requires the interrupt endpoint for slot change notifications, which fit in a few bytes. Both are compliant. The interval also differs (10ms vs 16ms).

### 4.4 Voltage Support

**Rust**: 5V only (bVoltageSupport=0x01). The STM32F469-DISCO board likely has a fixed 5V SIM slot.

**C**: 5V, 3V, 1.8V (bVoltageSupport=0x07). The NCN8025 SIM reader IC supports programmable voltage.

**Verdict**: Intentional platform difference.

---

## 5. Data Rate Negotiation

### 5.1 SetDataRateAndClockFrequency Comparison

| Aspect | Rust | C |
|--------|------|---|
| Clock frequency | **Ignored** (hardware fixed) | **Returns current** (doesn't change) |
| Data rate | **Actually adjusts BRR register** | **Returns current** (doesn't change) |
| Min rate | 9600 bps | N/A (always returns current) |
| Max rate | 5,000,000 bps | N/A (always returns current) |
| BRR validation | Rejects if BRR < 16 | N/A |
| Response | Actual (clock, rate) after adjustment | Current (clock, rate) unchanged |

### 5.2 Analysis

The C implementation claims AUTO_BAUD and AUTO_CLOCK features but does not actually change the data rate or clock. It always returns the currently active values. This is functionally a no-op.

The Rust implementation actually adjusts the USART BRR register to change the baud rate. The clock is fixed by hardware (APB1 prescaler). This is more honest — the feature actually works for the data rate component.

**Divergence**: Rust's clock frequency parameter is ignored (prefixed with `_`). The response returns the hardware-fixed clock value regardless of what the host requests. This should be documented or the response should indicate the actual clock used.

---

## 6. PIN/Secure Handling

### 6.1 Implementation Comparison

| Aspect | Rust | C |
|--------|------|---|
| PIN Verify (0x00) | **Full implementation** | CMD_NOT_SUPPORTED |
| PIN Modify (0x01) | **Full implementation** | CMD_NOT_SUPPORTED |
| PIN data structures | PinVerifyParams, PinModifyParams | Defined but unused |
| PIN entry UI | Touchscreen keypad | None |
| APDU construction | VERIFY (INS=0x20), CHANGE REF (INS=0x24) | N/A |
| Secure memory | volatile_clear on Drop | N/A |
| bPINSupport | 0x03 (Cherry), 0x00 (Gemalto) | 0x00 |
| Response deferral | Yes (main loop handles UI) | N/A |

### 6.2 PIN Flow (Rust Only)

1. Host sends PC_to_RDR_Secure with PIN operation data
2. CcidClass stores SecureState, returns no immediate response
3. Main loop detects active PIN entry, creates PinEntryContext
4. User enters PIN via touchscreen keypad
5. APDU built and transmitted to card
6. Response sent asynchronously

### 6.3 Analysis

The C reference defines PIN data structures in `ccid_proto.h` but never uses them. The Secure handler immediately returns CMD_NOT_SUPPORTED.

The Rust implementation has a complete PIN pad subsystem with:
- Parameter parsing per CCID Rev 1.1 Section 6.1.12
- State machine for PIN verify (single entry) and modify (3-step: old/new/confirm)
- Touchscreen UI (when display feature enabled)
- Secure memory handling (volatile_clear on buffer drop)
- APDU builders for VERIFY and CHANGE REFERENCE DATA

This is an intentional feature addition. The Cherry ST-2xxx device profile emulates a PIN pad reader; the Gemalto profiles do not support PIN.

---

## 7. Error Handling

### 7.1 Error Code Comparison

Both implementations use the same CCID error code set (0x00, 0xE0-0xFF). The codes map identically.

### 7.2 Error Response Routing

Both implementations route errors to the correct response message type based on the command that generated them:

| Command Type | Response Type |
|-------------|---------------|
| PowerOn/XfrBlock/Secure | DataBlock (0x80) |
| PowerOff/GetSlotStatus/Clock/etc. | SlotStatus (0x81) |
| Get/Set/ResetParameters | Parameters (0x82) |
| Escape | Escape (0x83) |

### 7.3 Error Handling Philosophy Comparison

| Aspect | Rust | C |
|--------|------|---|
| PPS failure | Graceful degradation | Card deactivation |
| IFSD failure | Graceful degradation | N/A (T=0 only) |
| ATR TCK mismatch | Rejected (T=1 only) | Logged, ATR accepted |
| USART errors (ORE/PE/FE) | Logged, byte consumed | WTIME expired -> deactivation |
| Card removal mid-operation | State reset | Context-dependent error |
| Concurrent commands | SLOT_BUSY rejection | SLOT_BUSY rejection |

### 7.4 Error Response Type Bug in C

The C implementation maps SetDataRateAndClockFrequency to a SlotStatus response, but the CCID spec (Section 6.1-4, Table 6.1-4) specifies it should use a DataBlock response. The Rust implementation also uses SlotStatus, so both have the same minor spec divergence.

---

## 8. Memory Management

### 8.1 Buffer Size Comparison

| Buffer | Rust | C |
|--------|------|---|
| CCID message max | 271 bytes (MAX_CCID_MESSAGE_LENGTH) | 272 bytes (dwMaxCCIDMessageLength) |
| USB packet size | 64 bytes | 64 bytes |
| ATR max | 33 bytes | 33 bytes |
| XfrBlock data max | 261 bytes | 260 bytes |
| T=1 block buffer | 257 bytes (NAD+PCB+LEN+INF+LRC) | N/A |
| PPS buffer | 6 bytes | 6 bytes |
| UART RX buffer | DMA/registers | 256 bytes (ring buffer) |
| PIN buffer | 16 digits | N/A |
| Message pool | Static arrays in CcidClass | 16 x 300-byte msgb pool |

### 8.2 ABDATADATASIZE Equivalent

Neither codebase defines a constant named `ABDATADATASIZE`. The effective values are:
- **Rust**: 261 bytes (MAX_CCID_MESSAGE_LENGTH - CCID_HEADER_SIZE = 271 - 10)
- **C**: 260 bytes (hardcoded XfrBlock length check)

The 1-byte difference (261 vs 260) is because Rust uses 271 total (10+261) while C uses 272 total (12+260, where 12 accounts for a larger header in some contexts). This is a minor accidental divergence — both are within the CCID spec's maximum of 261+10=271 for short APDU level.

### 8.3 Memory Architecture

| Aspect | Rust | C |
|--------|------|---|
| Allocation | Stack + static only | msgb pool (static pre-alloc) + talloc (heap for FSMs) |
| Heap | embedded-alloc dependency but unused | talloc for FSM instances |
| LTO | Enabled (codegen-units=1) | N/A (C compiler) |
| Optimization | Size (opt-level="z") | Standard |
| Flash target | 2048K | N/A (SAMD54) |
| RAM target | 256K | N/A (SAMD54) |

---

## 9. Code Organization

### 9.1 Architecture Comparison

```
RUST                              C (osmo-ccid-firmware)
─────                              ───────────────────────
main.rs (entry, display loop)      sysmoOCTSIM/main.c (main loop, USB, polling)
  +-- ccid.rs (CCID class)           +-- ccid_df.c (USB CCID function driver)
  +-- smartcard.rs (UART driver)      +-- ccid_device.c (command dispatch)
  +-- t1_engine.rs (T=1 engine)       +-- ccid_slot_fsm.c (ISO7816 bridge)
  +-- pps_fsm.rs (PPS FSM)           +-- iso7816_fsm.c (main FSM)
  +-- protocol_unit.rs (host-test)    +-- iso7816_3.c (Fi/Di tables)
  +-- device_profile.rs (profiles)    +-- ccid_proto.h (types, enums)
  +-- pinpad/ (PIN subsystem)         +-- cuart.c/h (UART abstraction)
      +-- mod.rs                         +-- cuart_driver_asf4_usart_async.c
      +-- state.rs                      +-- usb_descriptors.c
      +-- apdu.rs
      +-- ui.rs (display feature)
```

### 9.2 Key Architectural Differences

| Aspect | Rust | C |
|--------|------|---|
| State machine framework | Manual enum-based FSMs | osmo_fsm (hierarchical, event-driven) |
| Async model | Synchronous (blocking) | Asynchronous (event polling) |
| Abstraction layers | trait-based (SmartcardDriver, T1Transport) | Function pointers (ccid_ops, ccid_slot_ops) |
| Platform abstraction | HAL crate (stm32f4xx-hal) | ASF4 + custom cuart abstraction |
| Testability | Host-testable modules (lib.rs, protocol_unit.rs, pinpad/) | No host testing (all ARM-dependent) |
| Build system | Cargo + features | Atmel Start + ASF4 + Make |
| Device profiles | 3 compile-time profiles (Cargo features) | Single configuration |
| Cross-platform testing | Yes (tests/ directory, protocol_unit.rs) | No (ccid_host/ is a separate Linux program) |

### 9.3 ATR Parsing Duplication (Rust)

The `parse_atr()` function is duplicated between `smartcard.rs` (ARM-only) and `protocol_unit.rs` (host-testable). The logic is identical but the struct definitions differ (`AtrParams` defined in each file). This is an intentional trade-off for testability — the duplication allows protocol_unit to be compiled and tested on x86_64 without ARM dependencies.

---

## 10. Summary of Divergences

### 10.1 Intentional Divergences

| # | Divergence | Reason |
|---|-----------|--------|
| 1 | T=1 protocol support (Rust only) | Feature scope — Rust targets broader card support |
| 2 | PIN pad implementation (Rust only) | Feature addition — Cherry ST-2xxx emulation |
| 3 | Single slot vs 8 slots | Platform difference |
| 4 | Voltage support (5V vs 5V/3V/1.8V) | Hardware capability |
| 5 | Different MCU targets (STM32F469 vs SAMD54) | Platform difference |
| 6 | Device profiles (3 compile-time) | Emulation flexibility |
| 7 | Graceful PPS failure (Rust) | Robustness over strictness |
| 8 | Display/touch UI (Rust, feature-gated) | Hardware capability |
| 9 | Synchronous vs async command model | Simplicity vs scalability |
| 10 | Direct params vs proposed_pars | Simplicity + robustness trade-off |

### 10.2 Accidental Gaps in Rust

| # | Gap | Severity | Recommendation |
|---|-----|----------|----------------|
| 1 | No inverse convention support (TS=0x3F) | **High** — cards using inverse convention won't work | Add convention detection and byte inversion |
| 2 | No TCK verification for T=1 ATRs | ~~**Medium**~~ **FIXED** | ~~Add TCK XOR verification, reject on mismatch~~ | Implemented in `verify_atr_tck()` (smartcard.rs, protocol_unit.rs). ATR rejected if TCK mismatch and protocol is T=1. |
| 3 | P3==0 (256 bytes) not handled in T=0 | **Medium** — edge case for large response APDUs | Add explicit P3==0 -> 256 handling |
| 4 | Clock frequency parameter silently ignored | **Low** — response returns actual clock, so host can adapt | Document behavior or clamp to actual value |

### 10.3 Accidental Gaps in C

| # | Gap | Severity | Note |
|---|-----|----------|------|
| 1 | Abort command broken (always CMD_NOT_SUPPORTED) | **Low** — rarely used | Condition hardcoded to 0 |
| 2 | TCK mismatch only logged, not rejected | **Low** — T=0 only, TCK not required | Acceptable for T=0 |
| 3 | SetDataRateAndClockFrequency is a no-op | **Medium** — claims AUTO_BAUD/AUTO_CLOCK but doesn't change anything | Misleading feature advertisement |

### 10.4 Items for Follow-Up

1. **[cf-be8-gap-1]** Add inverse convention support to Rust ATR parser — HIGH priority
2. **[cf-be8-gap-2]** ~~Add TCK verification for T=1 ATRs in Rust — MEDIUM priority~~ **FIXED** — `verify_atr_tck()` rejects ATR on TCK mismatch for T=1
3. **[cf-be8-gap-3]** Handle P3==0 (256 bytes) in Rust T=0 transmit — MEDIUM priority
4. **[cf-be8-gap-4]** Consider proposed_pars pattern for Rust SetParameters — LOW priority (design choice)
