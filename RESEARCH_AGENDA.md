# CCID Reader — Research agenda and status

This document consolidates: **what to research**, **hypotheses** to validate, **what works**, **what is implemented**, and **what is still uncertain**. Use it to prioritize investigation and validation.

**How to use this:** Run host-side debug (`LIBCCID_ifdLogLevel=0x0F pcscd --foreground --debug`) and capture defmt logs when the device is attached via probe-rs/RTT; then work through the research items and tick off hypotheses as you confirm or refute them.

---

## 1. Things to research — ANSWERED

### SCARD_E_PROTO_MISMATCH (0x8010000F)

**Status: ANSWERED**

- **Where pcscd sets this**: `PCSC/src/winscard.c` in `SCardConnect`, `SCardReconnect`, and `SCardTransmit` functions.
- **Causes**:
  1. Requested protocols (`dwPreferredProtocols`) don't include valid bits (T0, T1, RAW)
  2. Preferred protocols don't match already negotiated `cardProtocol`
  3. `PHSetProtocol` returns `SET_PROTOCOL_WRONG_ARGUMENT`
  4. `IFDHSetProtocolParameters` returns `IFD_ERROR_NOT_SUPPORTED`

**Source**: `PCSC/src/winscard.c` lines 246, 398, 413, 553, 698, 713, 1557

### libccid protocol selection

**Status: ANSWERED**

- **Protocol selection sequence** (from `CCID/src/ifdhandler.c` and `CCID/src/towitoko/atr.c`):
  1. **PowerOn** → `CmdPowerOn` sends `PC_to_RDR_IccPowerOn` → receives ATR
  2. **ATR Parsing** → `ATR_GetDefaultProtocol` scans TD(i) bytes:
     - If TA2 present → specific mode (locked protocol)
     - First TD found → first offered protocol
     - No TD found → default to T=0
  3. **Protocol Check** → verify requested protocol in `dwProtocols`
  4. **PPS Negotiation** (if not auto-PPS) → `PPS_Exchange`
  5. **SetParameters** → send `PC_to_RDR_SetParameters` with protocol params
  6. **XfrBlock** → APDU exchange

**Source**: `CCID/src/ifdhandler.c` lines 731-1217, `CCID/src/towitoko/atr.c` lines 319-364

### ATR completeness

**Status: ANSWERED**

- **Truncated ATR handling**: If ATR is truncated but TD1 was received, protocol detection may succeed. If TD1 is missing, firmware defaults to T=0.
- **TCK check**: TCK is mandatory if any protocol other than T=0 is indicated (even if T=0 is also offered). TCK is XOR of all bytes from T0 to last byte before TCK.
- **libccid behavior**: Most middleware rejects ATR if TCK is missing or historical byte count K doesn't match remaining data.

**Source**: ISO 7816-3 Clause 8.2, 8.3

### GetParameters / SetParameters byte layout

**Status: ANSWERED — Our implementation is CORRECT**

- **CCID Spec layout**:
  - Command: `bProtocolNum` at byte 7, `abProtocolData` at bytes 10+
  - Response: Header (9 bytes) + `bProtocolNum` (byte 9) + `abProtocolData` (5 or 7 bytes)
  - `dwLength` = length of `abProtocolData` only (5 for T=0, 7 for T=1)

- **Our implementation** (`src/ccid.rs`):
  - `dwLength` set to 5 or 7 ✓
  - `bProtocolNum` at byte 9 ✓
  - `abProtocolData` at bytes 10+ ✓

**Source**: CCID Spec Rev 1.1 Section 6.1.7, 6.2.3; `osmo-ccid-firmware/ccid_common/ccid_proto.h`

### Voltage

**Status: ANSWERED**

- **Firmware behavior**: Descriptor advertises 5V/3V/1.8V (`bVoltageSupport = 0x07`) but firmware does NOT switch voltage.
- **Voltage mismatch consequences**:
  - 3V card @ 5V: Risk of damage or thermal issues
  - 5V card @ 3V: Card may not boot, appears mute
  - Wrong voltage can cause corrupted ATR → wrong protocol detection

- **Recommendation**: If hardware is fixed at one voltage, update `bVoltageSupport` to match. For 3V-only, use `0x02`.

**Source**: CCID Spec 1.1 Section 6.1.1; libccid `src/commands.c` lines 180-210

### NotifySlotChange usage

**Status: ANSWERED**

- **Message format**: `[0x50, bmSlotCCState]` where bits 0=present, 1=changed per slot
- **libccid behavior**: If interrupt endpoint present, uses it for card events (preferred). If absent, polls with `GetSlotStatus` every 200-400ms.
- **Our implementation**: Interrupt endpoint in descriptor, sends on card presence edge ✓

**Source**: CCID Spec 1.1 Section 6.3.1; `osmo-ccid-firmware/ccid_common/ccid_proto.h` lines 399-402

---

## 2. Hypotheses — VALIDATED

### H1 (Protocol mismatch): VALIDATED

**Hypothesis**: The mismatch is due to **protocol derivation**, not pins: we set `current_protocol` from ATR (TD1), but libccid may derive protocol from the same ATR differently.

**Finding**: Our `detect_protocol_from_atr()` correctly parses TD1. libccid's `ATR_GetDefaultProtocol` uses same logic:
1. Scan TD(i) for first protocol
2. Check TA2 for specific mode
3. Default to T=0 if nothing found

**Conclusion**: Protocol derivation logic is correct. If mismatch occurs, check:
- ATR is complete (not truncated)
- TD1 byte is being received correctly
- TA2 not present (would lock protocol)

### H2 (ATR truncation): POTENTIAL ISSUE

**Hypothesis**: If `read_atr()` returns early on byte timeout, we may send a **short ATR** lacking TD1.

**Finding**: Our `read_atr()` returns on timeout, which could truncate ATR. If TD1 is missing:
- Firmware defaults to T=0
- Card may be T=1
- Host may infer different protocol from partial ATR

**Mitigation**: Log full ATR hex via defmt. Compare with ATR from Gemalto reader for same card.

### H3 (SetParameters ordering): VALIDATED — CORRECT

**Hypothesis**: Host might send SetParameters before GetParameters response, or require specific order.

**Finding**: libccid sequence is:
1. PowerOn → ATR
2. (optional) GetParameters if needed
3. PPS negotiation (if not auto-PPS)
4. SetParameters with negotiated protocol
5. XfrBlock

Our implementation handles SetParameters independently, responds with current ATR-derived params. This is correct.

### H4 (Parameter bytes): VALIDATED — CORRECT

**Hypothesis**: Our `abProtocolData` might not match libccid's expected layout.

**Finding**: Our layout matches CCID spec:
- T=0: bmFindexDindex, bmTCCKST0, bGuardTimeT0, bWaitingIntegerT0, bClockStop (5 bytes) ✓
- T=1: adds bWaitingIntegersT1, bIFSC, bNadValue (7 bytes) ✓

**Source**: CCID Spec Table in Section 6.2.3

### H5 (Power cycle helped): VALIDATED

**Hypothesis**: The change from "Card is unresponsive" to "Card protocol mismatch" after adding power cycle suggests power-on path is now sufficient.

**Finding**: This is consistent with observations. Power cycle + longer delays ensure clean cold reset. Remaining issues are likely protocol/parameter negotiation, not electrical/timing.

---

## 3. What works (observed)

- **Build:** `cargo build --release --target thumbv7em-none-eabihf` succeeds.
- **Flash:** `st-flash --reset write ... 0x08000000` on remote host succeeds; device resets and runs.
- **USB enumeration:** `lsusb -d 076B:3021` shows "OmniKey AG CardMan 3021 / 3121"; descriptor (e.g. bNumEndpoints=3, dwProtocols=3, dwFeatures=0x000207B2) matches intent.
- **pcscd recognition:** `pcsc_scan` lists "OMNIKEY AG CardMan 3121 (001)" with "Card inserted" and ATR starting with `3B`.
- **Power-on and ATR:** With power cycle and longer delays, the reader reports a card and returns an ATR (at least TS=3B); no longer "Card is unresponsive" on connect.
- **14 CCID commands:** All are dispatched; PowerOn, PowerOff, GetSlotStatus, GetParameters, SetParameters, XfrBlock, IccClock, etc. Implemented handlers behave as in [VERIFICATION.md](VERIFICATION.md) and [REPORT.md](REPORT.md).

---

## 4. What is implemented (reference)

- **Descriptor:** VID:PID 076B:3021, strings "OMNIKEY AG" / "Smart Card Reader USB", CCID interface, 3 endpoints (Bulk IN/OUT, Int IN), dwFeatures 0x000207B2, dwProtocols 0x03, dwMaxCCIDMessageLength 271. See `src/ccid.rs` and [VERIFICATION.md](VERIFICATION.md) §1.
- **Slot and commands:** Single slot; cmd_busy; PowerOn (with full power cycle and Gemalto-style delays), PowerOff, GetSlotStatus, GetParameters, SetParameters, ResetParameters, XfrBlock (Short APDU only, ≤261 bytes), IccClock (accept and return SlotStatus); others return CMD_NOT_SUPPORTED with correct response type. See `src/ccid.rs` `handle_message` and handlers.
- **Smartcard path:** USART2 smartcard mode (PA2/PA4, PG10 RST, PC2 PRES, PC5 PWR); ATR read with leading-0x00 discard; PPS; T=0 (procedure bytes, GET RESPONSE, wrong Le) and T=1 (I/R/S, LRC, chaining, WTX S-block, IFSD). Protocol from ATR TD1; host can override via SetParameters. See `src/smartcard.rs` and `src/t1_engine.rs`.
- **Not implemented / limited:** Extended APDU rejected; a few optional commands (Escape, T0APDU, Secure, Mechanical) return CMD_NOT_SUPPORTED. SetDataRateAndClockFrequency, ResetParameters, IccClock (real clock control), and voltage check (bPowerSelect; 3V/1.8V rejected) are implemented. Host SetParameters is not applied to runtime baud/WI/IFSC (we use ATR-derived params; see [COMPARISON.md](COMPARISON.md)).

---

## 5. What we're still not sure about

### 5.1 Exact cause of 0x8010000F on specific cards

**Status: NEEDS TESTING**

The research shows multiple possible causes. To isolate:
1. Capture full ATR via defmt (verify complete, valid TCK)
2. Run `LIBCCID_ifdLogLevel=0x0F pcscd --foreground --debug` to see host's protocol selection
3. Compare ATR from our reader vs Gemalto for same card

### 5.2 Full ATR on device

**Status: NEEDS VERIFICATION**

We log ATR hex over defmt but haven't confirmed a full ATR (length and all bytes) on the remote device for the card that works in Gemalto. Truncated ATR is possible if byte timeout occurs too early.

### 5.3 Protocol actually used by the card

**Status: NEEDS VERIFICATION**

We don't have a definitive log showing "card is T=0" or "T=1" from our firmware for that same card. The `detect_protocol_from_atr()` function logs the detected protocol.

### 5.4 Whether libccid sends SetParameters for 076B:3021

**Status: ANSWERED — YES**

libccid sends SetParameters after PPS negotiation (if not auto-PPS). Since our dwFeatures includes `0x80` (Automatic PPS), libccid may skip explicit PPS but still sends SetParameters to confirm protocol parameters.

### 5.5 Voltage at the slot

**Status: NEEDS HARDWARE CHECK**

Unconfirmed; depends on hardware. If the board supplies a different voltage than the Gemalto, behaviour could differ even if protocol/params were correct.

**Recommendation**: Check hardware schematic. If fixed at 3V, update `bVoltageSupport` to `0x02` in descriptor.

### 5.6 RTT/defmt on remote

**Status: LOGGING AVAILABLE**

Logs (ATR, APDU, procedure bytes) are visible with probe-rs/RTT attached. Without that, we rely on host-side logs and behaviour.

### 5.7 Card damage from overvoltage (STM32 reader)

**Status: PENDING HARDWARE CHECK**

- **Risk:** Our firmware does not control slot voltage; the board supplies VCC (C1). If the STM32 board supplies **5 V** and the Seedkeeper is **3 V only**, the card may have been damaged when used in our reader.
- **Observation:** User reports the card no longer lights up when inserted in the Gemalto reader (previously it may have). That could mean: (1) pcscd not running / reader not powered, or (2) card damaged (e.g. by overvoltage in our reader).
- **Research items:** (a) What voltage does our STM32 smartcard slot actually supply (schematic or measurement)? (b) Seedkeeper datasheet: max VCC (3 V vs 5 V tolerant)? (c) Typical overvoltage damage symptoms; whether "no LED in known-good reader" indicates permanent damage.
- **Action:** Do not use 3 V–only cards in our reader until VCC is confirmed safe. When testing Gemalto, ensure pcscd is running and try to connect; if card still does not respond in Gemalto, treat as possibly damaged and document.

- **External investigation:** A copy-paste prompt for schematic/measurement or external LLM is in [VOLTAGE_INVESTIGATION_PROMPT.md](VOLTAGE_INVESTIGATION_PROMPT.md).

### 5.8 Seedkeeper in Gemalto first (unlock and read secret)

**Status: RESOLVED**

- **Goal:** Get the Seedkeeper working with the Gemalto reader first, then with the STM32 reader. Validate card and host stack (pysatochip).
- **Resolution:** SW=9C20 means **secure channel required**; the **host** (pysatochip) establishes it in passthrough mode. The reader (Gemalto or STM32) only forwards APDUs. Use **pysatochip** on the host to unlock and read.
- **Verified:** Full flow works on both Gemalto and STM32: ATR → SELECT SeedKeeper → GET_STATUS → secure channel (0x81) → VERIFY_PIN (1234) → LIST_SECRETS → EXPORT_SECRET. Reference secret: label "bacon", 24-word mnemonic. See [GEMALTO_KNOWN_GOOD_SUMMARY.md](GEMALTO_KNOWN_GOOD_SUMMARY.md), [PROTOCOL_SEEDKEEPER.md](PROTOCOL_SEEDKEEPER.md), [EXTERNAL_RESEARCH_REQUEST.md](EXTERNAL_RESEARCH_REQUEST.md) §5.

---

## 6. Key Research Sources

| Topic | Source |
|-------|--------|
| SCARD_E_PROTO_MISMATCH | `PCSC/src/winscard.c` lines 246, 398, 413, 553, 698, 713 |
| ATR_GetDefaultProtocol | `CCID/src/towitoko/atr.c` lines 319-364 |
| Protocol selection | `CCID/src/ifdhandler.c` lines 731-1217 |
| PPS negotiation | `CCID/src/towitoko/pps.c` |
| SetParameters format | CCID Spec 1.1 §6.1.7, 6.2.3 |
| Voltage handling | CCID Spec 1.1 §6.1.1; `CCID/src/commands.c` lines 180-210 |
| NotifySlotChange | CCID Spec 1.1 §6.3.1 |
| T=0 procedure bytes | ISO 7816-3/4 |
| T=1 block format | ISO 7816-3 |
| osmo-ccid reference | `osmo-ccid-firmware/ccid_common/` |

---

## 7. Next Steps for Debugging

1. **Capture full ATR**: Use `DEFMT_LOG=info probe-rs attach` to log complete ATR hex
2. **Host-side debug**: Run `LIBCCID_ifdLogLevel=0x0F pcscd --foreground --debug` 
3. **Compare readers**: Test same card in Gemalto vs our reader, compare ATR and protocol
4. **Verify voltage**: Check hardware supplies correct voltage to slot
5. **Test with known T=0 and T=1 cards**: Isolate if issue is protocol-specific

### 7.1 Defmt capture (remote STM32F469)

**Done**: probe-rs at `/home/ubuntu/.local/bin/probe-rs`; attach with ELF path:  
`probe-rs attach --chip STM32F469NIHx --target-output-file defmt=/tmp/defmt.log /tmp/ccid-reader.elf`

**Observed (before NACK fix)**: ATR consistently reported as **len=1** (only TS=0x3B). Second byte (T0) never received within timeout. This explains protocol mismatch: host gets truncated ATR and may infer wrong protocol or reject.

**Firmware changes applied (research-backed):**  
1. **NACK disabled during ATR** — In `power_on()`, CR3 NACK bit is cleared before `read_atr()`; re-enabled after ATR only for T=0 (see OPEN_RESEARCH_QUESTIONS.md §2.1, DETAILED_RESEARCH_REPORT.md §6.3).  
2. **USART error clearing** — `clear_usart_errors()` added; reads SR and clears ORE by reading DR; called at start of `read_atr()` and before each ATR byte.  
3. **ATR debug logging** — SR register value logged with each ATR byte (`ATR[i]: 0xXX SR=0xXXXX`).  
4. Existing: `SC_ATR_BYTE_TIMEOUT_MS` (1000 ms), 20 ms delay after TS, timeout info log.

**Full remote test:** Build and SCP completed; flash to remote failed at run time (ST-Link LIBUSB_ERROR_TIMEOUT). Re-run when probe is available: flash `ccid-reader.bin`, then `probe-rs attach` + `test_ccid_apdu.py "OMNIKEY"`, retrieve defmt and apdu logs.

---

## 8. Summary

Most research questions have been answered through code analysis and specification review. The remaining uncertainties are primarily about runtime behavior on specific hardware:

- **Known**: Protocol selection logic, SetParameters format, error causes, libccid behavior
- **Needs testing**: Full ATR capture, voltage verification, card-specific behavior
- **Implementation status**: Aligned with CCID spec and libccid expectations

Use this document with [RESEARCH.md](RESEARCH.md) for complete reference information.
