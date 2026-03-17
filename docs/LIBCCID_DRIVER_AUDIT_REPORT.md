# libccid/pcscd Host Driver Compatibility Audit Report

**Firmware**: ccid-firmware-rs v0.0.8
**Date**: 2026-03-17
**Issue**: cf-2pe
**Reference**: libccid source (https://salsa.debian.org/pcsclite/libccid)

---

## Executive Summary

The firmware is broadly compatible with Linux pcscd/libccid for Gemalto CT30 (08E6:3437) and K30 (08E6:3438) profiles. The Cherry ST-2xxx (046A:003E) profile has one significant descriptor mismatch (bInterfaceClass). Three actionable issues were identified that could cause operational problems under specific conditions.

| Severity | Count | Summary |
|----------|-------|---------|
| ~~CRITICAL~~ FIXED | 1 | ~~Gemalto escape 0x6A not handled~~ → Now returns valid GEMALTO_FIRMWARE_FEATURES |
| HIGH | 1 | Voltage support mismatch for CT30/K30 profiles (0x01 vs 0x07) |
| MEDIUM | 1 | Cherry ST-2xxx bInterfaceClass is 0x0B instead of real device's 0xFF |
| LOW | 4 | Minor descriptor mismatches that do not affect basic operation |

---

## 1. USB Descriptor Matching

### 1.1 VID/PID Registration in libccid

All three target VID:PID combinations are listed in libccid's `supported_readers.txt`:

| VID:PID | libccid Name | Firmware Profile | Status |
|---------|-------------|-----------------|--------|
| 0x08E6:0x3437 | Gemalto PC Twin Reader (IDBridge CT30) | `profile-gemalto-idbridge-ct30` | MATCH |
| 0x08E6:0x3438 | Gemalto USB Shell Token V2 (IDBridge K30) | `profile-gemalto-idbridge-k30` | MATCH |
| 0x046A:0x003E | Cherry GmbH SmartTerminal ST-2xxx | `profile-cherry-smartterminal-st2xxx` | MATCH |

**Verdict**: PASS. All three profiles use correct VID/PID values that libccid recognizes.

### 1.2 Interface Class

| Profile | Firmware | Real Device | Mismatch |
|---------|----------|-------------|----------|
| Cherry ST-2xxx | 0x0B (CCID) | 0xFF (proprietary) | YES |
| Gemalto CT30 | 0x0B (CCID) | 0x0B (CCID) | NO |
| Gemalto K30 | 0x0B (CCID) | 0x0B (CCID) | NO |

**Details**:
- Firmware hardcodes `CLASS_CCID = 0x0B` at `src/ccid.rs:86` for ALL profiles
- Real Cherry ST-2xxx uses `bInterfaceClass: 0xFF` (proprietary, not standard CCID)
- Real CT30 and K30 use `bInterfaceClass: 0x0B` (standard CCID) — matches firmware

**Impact**: The real Cherry ST-2xxx is NOT a standard CCID device at the USB level. It uses proprietary class 0xFF and is matched by libccid via VID:PID lookup only. With the firmware's 0x0B, the Linux kernel's `usbhid` or generic USB drivers won't claim it — instead the kernel `ccid` driver may bind directly (via class match) rather than going through the user-space pcscd VID:PID path. In practice, pcscd still works because it monitors the kernel ccid driver. However, this changes the driver binding behavior compared to the real device.

**Verdict**: MEDIUM. Not functionally broken but does not faithfully reproduce the real Cherry ST-2xxx USB topology.

---

## 2. CCID Descriptor Field-by-Field Comparison

### 2.1 Cherry SmartTerminal ST-2xxx (046A:003E)

| Field | Firmware | Real Device | Match |
|-------|----------|-------------|-------|
| bcdCCID | 0x0100 | 1.00 | PASS |
| bMaxSlotIndex | 0x00 | 0x00 | PASS |
| bVoltageSupport | 0x01 | 0x01 | PASS |
| dwProtocols | 0x00000003 | 0x00000003 | PASS |
| dwDefaultClock | 4000 kHz | 4000 kHz | PASS |
| dwMaximumClock | 8000 kHz | 8000 kHz | PASS |
| bNumClockSupported | 0 | 0 | PASS |
| dwDataRate | 10753 bps | 10753 bps | PASS |
| dwMaxDataRate | 344105 bps | 344105 bps | PASS |
| bNumDataRatesSupported | 0 | 0 | PASS |
| dwMaxIFSD | 254 | 254 | PASS |
| dwSynchProtocols | 0x00000000 | 0x00000000 | PASS |
| dwMechanical | 0x00000000 | 0x00000000 | PASS |
| dwFeatures | 0x000100BA | 0x000100BA | PASS |
| dwMaxCCIDMessageLength | 270 | 270 | PASS |
| bClassGetResponse | 0xFF | 0xFF | PASS |
| bClassEnvelope | 0xFF | 0xFF | PASS |
| wLcdLayout | 0x0000 | 0x0000 | PASS |
| bPINSupport | 0x03 | 0x03 | PASS |
| bMaxCCIDBusySlots | 1 | 1 | PASS |

**Cherry profile score: 20/21 PASS** (bInterfaceClass mismatch noted above)

### 2.2 Gemalto IDBridge CT30 (08E6:3437)

| Field | Firmware | Real Device | Match |
|-------|----------|-------------|-------|
| bcdCCID | 0x0101 | 1.01 | PASS |
| bMaxSlotIndex | 0x00 | 0x00 | PASS |
| bVoltageSupport | **0x01** | **0x07** | **FAIL** |
| dwProtocols | 0x00000003 | 0x00000003 | PASS |
| dwDefaultClock | 4800 kHz | 4800 kHz | PASS |
| dwMaximumClock | 4800 kHz | 4800 kHz | PASS |
| bNumClockSupported | 0 | 0 | PASS |
| dwDataRate | 12903 bps | 12903 bps | PASS |
| dwMaxDataRate | 825806 bps | 825806 bps | PASS |
| bNumDataRatesSupported | **0** | **53** | MISMATCH |
| dwMaxIFSD | 254 | 254 | PASS |
| dwSynchProtocols | 0x00000000 | 0x00000000 | PASS |
| dwMechanical | 0x00000000 | 0x00000000 | PASS |
| dwFeatures | 0x00010230 | 0x00010230 | PASS |
| dwMaxCCIDMessageLength | 271 | 271 | PASS |
| bClassGetResponse | 0x00 | 0x00 | PASS |
| bClassEnvelope | 0x00 | 0x00 | PASS |
| wLcdLayout | 0x0000 | 0x0000 | PASS |
| bPINSupport | 0x00 | 0x00 | PASS |
| bMaxCCIDBusySlots | 1 | 1 | PASS |

**CT30 profile score: 18/21 PASS, 1 FAIL, 2 MISMATCH**

### 2.3 Gemalto IDBridge K30 (08E6:3438)

| Field | Firmware | Real Device | Match |
|-------|----------|-------------|-------|
| bcdCCID | 0x0101 | 1.01 | PASS |
| bMaxSlotIndex | 0x00 | 0x00 | PASS |
| bVoltageSupport | **0x01** | **0x07** | **FAIL** |
| dwProtocols | 0x00000003 | 0x00000003 | PASS |
| dwDefaultClock | 4800 kHz | 4800 kHz | PASS |
| dwMaximumClock | 4800 kHz | 4800 kHz | PASS |
| bNumClockSupported | 0 | 0 | PASS |
| dwDataRate | 12903 bps | 12903 bps | PASS |
| dwMaxDataRate | 825806 bps | 825806 bps | PASS |
| bNumDataRatesSupported | **0** | **53** | MISMATCH |
| dwMaxIFSD | 254 | 254 | PASS |
| dwSynchProtocols | 0x00000000 | 0x00000000 | PASS |
| dwMechanical | 0x00000000 | 0x00000000 | PASS |
| dwFeatures | 0x00010230 | 0x00010230 | PASS |
| dwMaxCCIDMessageLength | 271 | 271 | PASS |
| bClassGetResponse | 0x00 | 0x00 | PASS |
| bClassEnvelope | 0x00 | 0x00 | PASS |
| wLcdLayout | 0x0000 | 0x0000 | PASS |
| bPINSupport | 0x00 | 0x00 | PASS |
| bMaxCCIDBusySlots | 1 | 1 | PASS |

**K30 profile score: 18/21 PASS, 1 FAIL, 2 MISMATCH** (identical to CT30)

---

## 3. Detailed Findings

### 3.1 [FIXED] Gemalto Escape 0x6A Firmware Features Query

**Severity**: FIXED (was CRITICAL)
**Affected profiles**: CT30, K30
**Firmware**: `src/ccid.rs` — `handle_escape()`
**libccid**: `src/ccid.c` — `ccid_open_hack_post()` → `set_gemalto_firmware_features()`

**Description**: During device initialization, libccid sends escape command `{0x6A}` (GET_FIRMWARE_FEATURES) to ALL readers with VID `0x08E6` (Gemalto vendor). This queries a `GEMALTO_FIRMWARE_FEATURES` struct containing PIN operation capabilities, display features, and bug workarounds.

The firmware now implements escape 0x6A for Gemalto profiles (VID 0x08E6), returning a minimal valid `GEMALTO_FIRMWARE_FEATURES` response with `bNumberMessageFix = 1`. This suppresses the `has_gemalto_modify_pin_bug()` workaround. All other capability fields remain 0 since CT30/K30 lack PIN pads and displays. Non-Gemalto profiles and non-0x6A escape codes continue to return `CMD_NOT_SUPPORTED`.

### 3.2 [HIGH] Voltage Support Mismatch for CT30/K30

**Severity**: HIGH
**Affected profiles**: CT30, K30
**Firmware**: `src/device_profile.rs:372` (BASE_PROFILE), not overridden by CT30/K30 profiles
**Reference**: `reference/CCID/readers/Gemalto_IDBridge_CT30.txt:22` — `bVoltageSupport: 0x07`

**Description**: The firmware reports `bVoltageSupport = 0x01` (5V only) for CT30 and K30 profiles. The real devices report `0x07` (5V + 3V + 1.8V).

**Consequences**:
1. libccid calls `IFDHPowerICC()` with `dwVoltage = 0` (auto-select) by default. Since `bVoltageSupport & 0x07` indicates only 5V, pcscd will only try 5V. This works for most SIM/USIM cards but fails for cards that strictly require 3V.
2. If pcscd is configured with `AutoVoltage = true` (the default), it iterates through supported voltages. With 0x01, it only tries 5V and gives up if the card doesn't respond.
3. The firmware's `handle_power_on()` at `src/ccid.rs:621-629` explicitly rejects `bPowerSelect` values 0x02 (3V) and 0x03 (1.8V), returning `CMD_NOT_SUPPORTED`.

**Recommendation**: For CT30/K30 profiles, override `voltage_support: 0x07` in the profile definition. Note: this is a descriptor-only change — the actual hardware voltage selection would need a corresponding `handle_power_on` change to accept 3V/1.8V requests, which depends on the STM32 board's voltage regulator capabilities.

### 3.3 [MEDIUM] Cherry ST-2xxx bInterfaceClass Mismatch

**Severity**: MEDIUM
**Affected profiles**: Cherry ST-2xxx
**Firmware**: `src/ccid.rs:86,1645` (hardcoded `CLASS_CCID = 0x0B`)
**Reference**: `reference/CCID/readers/CherrySmartTerminalST2XXX.txt:12` — `bInterfaceClass: 0xFF`

**Description**: The real Cherry ST-2xxx uses USB class 0xFF (vendor-specific/proprietary), NOT the standard CCID class 0x0B. The firmware hardcodes 0x0B for all profiles via `CLASS_CCID`.

**Consequences**:
1. With 0x0B, the Linux kernel's `ccid` driver (usbcore class-based binding) will claim the device directly. With the real 0xFF, the device is invisible to class-based drivers and only matched by pcscd's user-space VID:PID scanner.
2. This changes the device access path: kernel ccid driver → pcscd (via /dev/bus/usb) vs. direct pcscd → USB device. Both paths ultimately work, but the kernel ccid driver path may have different permission models and hotplug behavior.
3. udev rules for CCID devices (`ACTION=="add", SUBSYSTEM=="usb", ENV{ID_USB_CLASS_FROM_DEVICE}=="0b"`) will match the firmware but not the real device.

**Recommendation**: If exact behavioral reproduction is required, the Cherry profile should override `bInterfaceClass` to 0xFF. This would require making the class configurable per profile rather than hardcoded in `ccid.rs:1645`.

### 3.4 [LOW] bNumDataRatesSupported Mismatch for CT30/K30

**Severity**: LOW
**Affected profiles**: CT30, K30
**Firmware**: Reports `bNumDataRatesSupported = 0` (continuous range)
**Reference**: Real CT30/K30 report 53 discrete data rates

**Consequences**: With `bNumDataRatesSupported = 0`, libccid treats the data rate range as continuous between `dwDataRate` and `dwMaxDataRate`. It will compute any rate in that range rather than selecting from a discrete list. The real device only supports 53 specific rates. In practice, libccid's `IFDHSetProtocolParameters` checks `dwMaxDataRate` as the upper bound regardless, so this rarely causes issues.

### 3.5 [LOW] GET_CLOCK_FREQUENCIES/GET_DATA_RATES Hardcoded

**Severity**: LOW
**Affected profiles**: All
**Firmware**: `src/ccid.rs:154,157` — Returns fixed 4000 kHz and 10752 bps
**Reference**: Real CT30/K30 support GET_CLOCK_FREQUENCIES (returns 4800 kHz) and GET_DATA_RATES (returns 53 rates). Real Cherry ST-2xxx does NOT support these requests (times out).

**Consequences**: The returned values don't match any profile's actual capabilities. However, libccid uses `bNumClockSupported = 0` and `bNumDataRatesSupported = 0` to indicate "ignore these requests," so the returned values are largely unused in practice.

### 3.6 [LOW] Product String Mismatch for CT30/K30

**Severity**: LOW
**Affected profiles**: CT30, K30
**Firmware**: `"IDBridge CT30"` / `"IDBridge K30"` at `src/device_profile.rs:479,520`
**Reference**: Real device returns `"USB SmartCard Reader"` (generic)

**Consequences**: No functional impact — libccid matches by VID:PID, not product string. However, tools like `lsusb` or `pcsc_scan` will show different product names than the real devices.

### 3.7 [LOW] bcdDevice (Firmware Release) Not Set Per Profile

**Severity**: LOW
**Affected profiles**: All
**Firmware**: BASE_PROFILE sets `device_release: 0x0100` (1.00), not overridden
**Reference**: Cherry ST-2xxx = 6.01, CT30 = 2.01, K30 = 2.00

**Consequences**: The ZLP fixup in libccid (`ccid_open_hack_pre`) only triggers when `IFD_bcdDevice == 0x0200`. With 0x0100, this fixup is skipped. For the K30 (real device has 2.00), this means the firmware won't get the USB 3.0 ZLP workaround that the real K30 does. In practice, this is unlikely to cause issues on full-speed USB.

---

## 4. CCID Message Format Compatibility

### 4.1 Message Type Constants

All PC_to_RDR and RDR_to_PC message type constants match the CCID Rev 1.1 specification and libccid's `ccid.h` definitions exactly.

| Category | Count | Status |
|----------|-------|--------|
| PC_to_RDR commands | 12 defined | PASS |
| RDR_to_PC responses | 6 defined | PASS |
| Class-specific requests | 3 defined | PASS |

### 4.2 bmCommandStatus Encoding

Firmware: `(cmd_status << 6) | icc_status` at `src/ccid.rs:513`

| Status | Firmware | libccid | Match |
|--------|----------|---------|-------|
| NO_ERROR (0x00) | 0x00 | 0x00 | PASS |
| FAILED (0x01) | 0x40 | 0x40 | PASS |
| TIME_EXTENSION (0x80) | 0x80 | 0x80 | PASS |

**Verdict**: PASS. Bit layout matches CCID spec Table 6.2-2.

### 4.3 bError Codes

All error codes defined in the firmware (`src/ccid.rs:123-139`) match the CCID spec and libccid's `ccid.h`. Notable codes:

| Code | Firmware | libccid | Used In |
|------|----------|---------|---------|
| 0xFE | ICC_MUTE | CMD_ICC_MUTE | No card / power-on failure |
| 0xFB | HW_ERROR | CMD_HW_ERROR | Data rate negotiation failure |
| 0xEF | PIN_CANCELLED | PIN_CANCELED | PIN entry cancelled |
| 0xF0 | PIN_TIMEOUT | PIN_TIMEOUT | PIN entry timeout |
| 0xFF | CMD_ABORTED | CMD_ABORTED | Command aborted |

**Verdict**: PASS. All error codes are spec-compliant.

### 4.4 bChainParameter

The firmware sets `bChainParameter = 0` in all responses (`src/ccid.rs:647,667,919,1066`). This is correct for TPDU-level exchange where the host handles chaining. For short/extended APDU levels, this field would carry chain state, but the firmware uses TPDU level exclusively.

**Verdict**: PASS.

---

## 5. Feature Flags (dwFeatures) Analysis

### 5.1 Cherry ST-2xxx: dwFeatures = 0x000100BA

| Bit | Flag | Firmware | Real Device | Match |
|-----|------|----------|-------------|-------|
| 1 | Auto ATR config | SET | SET | PASS |
| 2 | Auto activation | NOT SET | NOT SET | PASS |
| 3 | Auto voltage | SET | SET | PASS |
| 5 | Auto baud | SET | SET | PASS |
| 7 | Auto PPS | SET | SET | PASS |
| 8 | Clock stop | SET | SET | PASS |
| 16 | TPDU level | SET | SET | PASS |

**Verdict**: PASS. Exact match with real device.

### 5.2 CT30/K30: dwFeatures = 0x00010230

| Bit | Flag | Firmware | Real Device | Match |
|-----|------|----------|-------------|-------|
| 4 | Auto clock | SET | SET | PASS |
| 5 | Auto baud | SET | SET | PASS |
| 9 | NAD other | SET | SET | PASS |
| 16 | TPDU level | SET | SET | PASS |

**Verdict**: PASS. Exact match with real device.

### 5.3 libccid Feature-Based Behavior

libccid routes `CmdXfrBlock` based on `dwFeatures & EXCHANGE_MASK`:

| Exchange Level | Value | libccid Routing | Firmware |
|---------------|-------|-----------------|----------|
| CHARACTER | 0x00000 | T=0: CHAR_T0, T=1: TPDU_T1 | Not used |
| TPDU | 0x10000 | T=0: TPDU_T0, T=1: TPDU_T1 | All profiles |
| SHORT_APDU | 0x20000 | T=0: TPDU_T0 only | Not used |
| EXTENDED_APDU | 0x40000 | APDU_extended (chaining) | Not used |

All profiles use TPDU level, which means libccid sends raw T=1 blocks via XfrBlock and the firmware's `t1_engine.rs` handles the T=1 framing. This is correct and matches the real devices.

**Auto IFSD (bit 10)**: Not set in any profile. Correct — with TPDU level, IFSD negotiation is handled by the firmware's T=1 engine via S-block, not by libccid.

**Auto PPS (bit 7)**: Set in Cherry profile only. With this flag, libccid skips PPS negotiation and SetParameters, letting the reader handle PPS autonomously. The firmware's `smartcard.rs:472` does call `negotiate_pps_fsm()` during power-on, so this is consistent.

---

## 6. ATR Parsing Compatibility

### 6.1 ATR Generation

The firmware reads ATR from the card via USART2 at `src/smartcard.rs:540-608`. The ATR is passed verbatim to the host in the `RDR_to_PC_DataBlock` response (`src/ccid.rs:649-652`).

**Maximum ATR length**: 33 bytes (`SC_ATR_MAX_LEN` at `src/smartcard.rs:36`). Matches CCID spec maximum.

### 6.2 Protocol Selection from ATR

The firmware parses ATR to detect T=0 vs T=1 at `src/smartcard.rs:611-645`:
- Reads T0 byte, finds TD1
- Extracts protocol from TD1's lower nibble
- Defaults to T=0 if no TD1 found

libccid parses the same ATR in `IFDHSetProtocolParameters()`:
- Checks TA1 for Fi/Di parameters
- Checks TA2 for specific mode
- Checks TD1+ for protocol selection

**Verdict**: PASS. ATR is passed verbatim to the host; both sides parse independently.

### 6.3 ATR-Based Parameter Configuration

With `FEAT_AUTO_PARAM_ATR` (bit 1) set, libccid expects the reader to auto-configure timing parameters from the ATR. The firmware does this in `smartcard.rs:power_on()`:
1. Parses ATR → `AtrParams`
2. Negotiates PPS if needed
3. Configures USART baud rate from Fi/Di

**Verdict**: PASS. Auto parameter configuration is correctly implemented.

---

## 7. Data Rate Negotiation

### 7.1 SetDataRateAndClockFrequency

**Firmware**: `src/ccid.rs:725-771`
**libccid**: `ifdhandler.c` → `IFDHSetProtocolParameters()`

The firmware handles this command and returns actual clock/rate values. However:
- Clock frequency is fixed by hardware (`_clock_hz` parameter ignored at `smartcard.rs:289`)
- Only baud rate is applied, clamped to 9600–5,000,000 bps

**libccid negotiation flow**:
1. Parse ATR for Fi/Di
2. Compute target: `card_baudrate = 1000 * dwDefaultClock * D / F`
3. Check against `dwMaxDataRate`
4. If rate is acceptable, send `SetDataRateAndClockFrequency`
5. If `CCID_CLASS_AUTO_PPS_PROP` or `CCID_CLASS_AUTO_PPS_CUR` set, skip PPS

**Consequences**: The firmware returns the actual (possibly clamped) rate, which libccid uses. Since dwFeatures has AUTO_BAUD set, libccid may skip explicit rate setting and rely on the firmware's auto-configuration. This works correctly.

### 7.2 Class-Specific Requests

| Request | Firmware Response | Real CT30 | Real Cherry |
|---------|------------------|-----------|-------------|
| GET_CLOCK_FREQUENCIES (0x02) | 4000 kHz | 4800 kHz | Timeout (not supported) |
| GET_DATA_RATES (0x03) | 10752 bps | 53 rates | Timeout (not supported) |

These values are hardcoded constants (`src/ccid.rs:154,157`) and don't vary per profile. Since `bNumClockSupported = 0` and `bNumDataRatesSupported = 0`, libccid treats these as informational only.

**Verdict**: LOW. Functionally adequate but not profile-accurate.

---

## 8. Known libccid Quirks

### 8.1 Gemalto Readers (CT30, K30)

| Quirk | libccid Behavior | Firmware Status |
|-------|-----------------|-----------------|
| ZLP fixup (bcdDevice == 0x0200) | Sets `zlp = true` for USB 3 compat | NOT triggered (firmware bcdDevice = 1.00) |
| Escape 0x6A firmware features | Queries PIN/display capabilities | **Handled** (returns minimal features struct) |
| Escape 0x1F 0x02 TPDU→APDU switch | Switches from TPDU to SHORT_APDU | NOT handled (not applicable with DRIVER_OPTION) |
| Gemalto modify PIN bug | Fakes bNumberMessage=3 if no bNumberMessageFix | **Not triggered** (bNumberMessageFix = 1) |

### 8.2 Cherry ST-2xxx

| Quirk | libccid Behavior | Firmware Status |
|-------|-----------------|-----------------|
| SecurePINVerify no-display | Forces bNumberMessage=3, bMsgIndex123=0 | N/A (firmware has display feature) |
| SecurePINModify no-display | Forces bNumberMessage=3, bMsgIndex123=0 | N/A (firmware has display feature) |

**Note**: The Cherry quirks only apply when the reader has NO display (`wLcdLayout = 0x0000`). The real Cherry ST-2xxx has `wLcdLayout = 0x0000` in the descriptor (even though the physical device has a display). The firmware also sets `wLcdLayout = 0x0000`. If the `display` feature is enabled, the firmware provides a touchscreen UI, but libccid still applies the no-display workaround based on the descriptor.

### 8.3 No Bogus Firmware Entries

None of the three target readers appear in libccid's bogus firmware list (`ccid_usb.c`). No additional descriptor fixups are applied.

---

## 9. pcscd Daemon Compatibility

### 9.1 IFDHandler Interface

The firmware does not implement the IFDHandler interface — that is a user-space library interface between pcscd and the driver. The firmware is the USB device side; pcscd + libccid is the host side. Compatibility is ensured through:
1. USB descriptor matching (VID:PID in supported_readers.txt) — PASS
2. CCID class descriptor compliance — PASS (with noted exceptions)
3. CCID bulk protocol message format — PASS
4. Interrupt endpoint for slot change notifications — PASS

### 9.2 Hotplug / udev

With `bInterfaceClass = 0x0B`, the firmware triggers standard CCID udev rules. pcscd monitors the kernel ccid driver and auto-starts. This works correctly for CT30/K30. For Cherry ST-2xxx (real device uses 0xFF), the behavior differs slightly (see finding 3.3).

### 9.3 pcscd Socket Protocol

The firmware does not interact with the pcscd socket protocol — that is between client applications (e.g., `pkcs11`, `opensc`) and the pcscd daemon. No firmware-side requirements.

---

## 10. Compatibility Matrix Summary

| Feature | Cherry ST-2xxx | CT30 | K30 |
|---------|:-:|:-:|:-:|
| USB VID:PID match | PASS | PASS | PASS |
| bInterfaceClass | **WARN** (0x0B vs 0xFF) | PASS | PASS |
| CCID descriptor fields | 20/21 PASS | 18/21 PASS | 18/21 PASS |
| dwFeatures exact match | PASS | PASS | PASS |
| Message format | PASS | PASS | PASS |
| Error codes | PASS | PASS | PASS |
| ATR passthrough | PASS | PASS | PASS |
| Data rate negotiation | PASS | PASS | PASS |
| SetParameters (libccid quirk) | PASS | PASS | PASS |
| Escape 0x6A (Gemalto) | N/A | PASS (fixed) | PASS (fixed) |
| Voltage support | PASS | **FAIL** (0x01 vs 0x07) | **FAIL** (0x01 vs 0x07) |
| Slot change interrupt | PASS | PASS | PASS |
| PIN verify/modify | PASS (with display) | N/A | N/A |
| Overall | **USABLE** | **USABLE** | **USABLE** |

---

## 11. Recommendations

### Immediate Actions

1. ~~**Implement Gemalto escape 0x6A**~~ for CT30/K30 profiles. ~~Return a minimal `GEMALTO_FIRMWARE_FEATURES` response with `bNumberMessageFix = 1` to suppress the modify PIN workaround.~~ **DONE** — implemented in `handle_escape()`.

2. **Override voltage_support to 0x07** for CT30/K30 profiles in `device_profile.rs`. Also update `handle_power_on()` to accept 3V/1.8V bPowerSelect values (or return a more appropriate error code).

### Future Improvements

3. **Make bInterfaceClass configurable per profile** to allow Cherry ST-2xxx to use 0xFF for exact device reproduction.

4. **Make GET_CLOCK_FREQUENCIES and GET_DATA_RATES profile-aware** so they return values matching the active profile's capabilities.

5. **Set bcdDevice per profile** to match real device firmware versions (Cherry: 6.01, CT30: 2.01, K30: 2.00).

6. **Consider implementing bNumDataRatesSupported = 53** for CT30/K30 with the real device's data rate list, or document why continuous range (0) is preferred.

---

## Appendix A: Source File References

| Component | Firmware File | Key Lines |
|-----------|--------------|-----------|
| USB descriptors | `src/ccid.rs` | 1640-1660 |
| CCID class descriptor | `src/device_profile.rs` | 224-320 |
| Device profiles | `src/device_profile.rs` | 359-540 |
| Feature flags | `src/device_profile.rs` | 39-98 |
| CCID message types | `src/ccid.rs` | 25-68 |
| Error codes | `src/ccid.rs` | 123-139 |
| Power on / ATR | `src/ccid.rs` | 586-668 |
| SetParameters | `src/ccid.rs` | 1017-1087 |
| Data rate negotiation | `src/ccid.rs` | 725-771 |
| Escape handling | `src/ccid.rs` | `handle_escape()` |
| Class requests | `src/ccid.rs` | 1750-1802 |
| ATR parsing | `src/smartcard.rs` | 88-163, 540-608 |
| Protocol detection | `src/smartcard.rs` | 611-645 |
| T=1 engine | `src/t1_engine.rs` | 160-296 |
| PPS negotiation | `src/pps_fsm.rs` | Full file |
| USB identity | `src/usb_identity.rs` | Full file |

## Appendix B: libccid Source References

| Component | libccid File | Function/Section |
|-----------|-------------|-----------------|
| Reader list | `readers/supported_readers.txt` | 08E6:3437, 08E6:3438, 046A:003E |
| Descriptor parsing | `src/ccid_usb.c` | `get_ccid_descriptor()` |
| Gemalto quirks | `src/ccid.c` | `ccid_open_hack_pre()`, `ccid_open_hack_post()` |
| Escape 0x6A | `src/ccid.c` | `set_gemalto_firmware_features()` |
| Protocol params | `src/ifdhandler.c` | `IFDHSetProtocolParameters()` |
| XfrBlock routing | `src/commands.c` | `CmdXfrBlock*()` |
| PIN verify/modify | `src/commands.c` | `SecurePINVerify()`, `SecurePINModify()` |
| Cherry PIN quirks | `src/commands.c` | CHERRYST2000 conditionals |
| Data rate calc | `src/ifdhandler.c` | `card_baudrate = 1000*clock*D/F` |
