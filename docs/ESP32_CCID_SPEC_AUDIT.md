# esp32-ccid CCID Rev 1.1 Specification Compliance Audit

**Document Purpose**: Audit of the `firmware/esp32-ccid` firmware against USB CCID
Rev 1.1 §6.1 (PC_to_RDR commands) and §6.2/§6.3 (responses), as carried over the
GemPC Twin serial transport (UART / USB-CDC) instead of USB bulk endpoints.

**Spec Reference**: [DWG_Smart-Card_CCID_Rev110.pdf](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf)

**Sibling audit**: `docs/CCID_SPEC_AUDIT.md` (STM32 USB firmware — format model
for this document).

**Source of truth** (this table is generated from, and should be re-verified
against):

- Dispatch: `firmware/esp32-ccid/src/ccid_handler.rs` — `CcidHandler::process_command`
- Message constants: `crates/ccid-protocol/src/types.rs`, re-exported by
  `firmware/esp32-ccid/src/ccid_types.rs`
- Serial framing: `crates/ccid-transport-serial/src/lib.rs`
- Serving core: `firmware/esp32-ccid/src/ccid_serial_server.rs`
- Wire battery: `tests/hardware/nfc/conformance_battery.py`

**Firmware identity**: emulated GemPC Twin (Gemplus 0x08E6:0x3437 identity,
`dwFeatures = 0x00010270`, `dwMaxCCIDMessageLength = 271`), recognized by
`pcscd`/`libccidtwin` without custom drivers. All mains route through the single
`CcidSerialServer` serving core since the #90 consolidation.

---

## Serial Framing Layer (transport below CCID)

The GemPC Twin wire format wraps every CCID message:

```
[SYNC=0x03] [CTRL: ACK=0x06 | NAK=0x15] [CCID message (10-byte header + payload)] [LRC]
```

LRC = XOR of all preceding bytes (including SYNC and CTRL). Host commands are
**echoed verbatim** before the framed response (two writes per command).

| Mechanism | Implementation | Verification |
|---|---|---|
| SYNC byte (0x03) | `FrameParser::ParserState::WaitSync` (`ccid-transport-serial/src/lib.rs:46`) | 25 unit tests + 87-case wire fuzz |
| CTRL ACK/NAK validation | `WaitCtrl` state; invalid CTRL → `FrameError::InvalidCtrl`; NAK → `NakReceived` | unit tests + fuzz cases "NAK ctrl frame", "no SYNC garbage" |
| dwLength overflow guard | payload > 261 → `FrameError::Overflow` (short-APDU max) | `test_parser_overflow_when_payload_too_large`, fuzz "oversized dwLength" |
| LRC check | `CheckLrc` state; mismatch → `FrameError::InvalidLrc` | `test_corrupted_lrc_every_bit_position`, fuzz "bad LRC" |
| NAK frame emission | `build_nak_frame` → `03 15 16` | `test_build_nak_frame`; wire-fuzzed |
| Echo-before-response | `received_frame_bytes()` snapshot survives parser reset | `ccid_serial_server` tests, on-target 13/13 |
| Desync recovery | stray SYNC in `WaitCtrl` restarts frame collection; inter-byte stall > 10 ticks resets parser (fuzz-proven 2026-10-09) | fuzz suite + 87-case battery resync proof |
| `NotifySlotChange` prefix suppression | stray `0x50` while `WaitSync` resets (echo path, not a frame start) | `test_parser_ignores_slot_change_notification_prefix` |

**The 87-case fuzz battery** (`conformance_battery.py::fuzz_battery`): 7 fixed
malformed cases (bad LRC, truncated header, no-SYNC garbage, SYNC+garbage,
oversized dwLength, lone SYNC, NAK ctrl frame) + 80 seeded-random rounds = 87
cases per reader, each followed by a valid GetSlotStatus resync proof. Run
against BOTH bench readers (nucula `pn7160-ccid`, m5stick MFRC522): 87/87, zero
wedges on each (2026-10-09).

**Wire-behavior profiles** (`ccid_serial_server.rs::ServeConfig`): UART mains
NAK malformed frames and emit in-stream `NotifySlotChange` (libccidtwin
ReadSerial expects both); the USB-CDC main silently drops parse errors and never
notifies (its 13/13 on-target tests passed with exactly these semantics). Do not
"fix" either profile without re-validating against libccidtwin on hardware.

---

## PC_to_RDR Command Coverage (CCID Rev 1.1 §6.1)

Dispatch: `ccid_handler.rs:106-124` (`process_command`). All commands reach it
through `CcidSerialServer::feed_byte`. Unknown/unhandled types fall to the
catch-all (`ccid_handler.rs:115`): `RDR_to_PC_SlotStatus` with
`bmCommandStatus=failed`, `bError=CMD_NOT_SUPPORTED (0x00)`.

| § | Command | Type | Status | Firmware path (ccid_handler.rs) | Wire verification | Known divergences |
|---|---|---|---|---|---|---|
| 6.1.1 | IccPowerOn | 0x62 | ✅ implemented | `handle_power_on` (:160) → DataBlock w/ ATR | battery (no-card FAIL path), host tests, fuzz, 13/13 on-target | `bPowerSelect` ignored (NFC field power not selectable) while `VOLTAGE_SUPPORT=0x07` advertised; no-card → SlotStatus failed + `CMD_NOT_SUPPORTED`; activation failure → SlotStatus failed + `HW_ERROR (0xFB)` |
| 6.1.2 | IccPowerOff | 0x63 | ✅ implemented | `handle_power_off` (:219) → SlotStatus | battery, host tests, fuzz | always succeeds (idempotent DESELECT); sets PresentInactive without polling (ISO 14443 state-machine reason, :222) |
| 6.1.3 | GetSlotStatus | 0x65 | ✅ implemented | `handle_get_slot_status` (:243) → SlotStatus | battery, 1000× soak, fuzz, host tests | presence cache fed by interval-gated polls (500 ms default), not live hardware reads |
| 6.1.4 | XfrBlock | 0x6F | ✅ implemented | `handle_xfr_block` (:256) → DataBlock | battery (no-card FAIL path), host tests, fuzz | see XfrBlock notes below (#49/#50, PPS echo, inactive-slot `bError=0x05`) |
| 6.1.5 | GetParameters | 0x6C | ✅ implemented | `write_parameters` (:484) → Parameters | host tests (T=0 + T=1), fuzz | returns static defaults (`ccid_core::params::default_params`), NOT ATR-derived (contrast STM32 audit) |
| 6.1.6 | ResetParameters | 0x6D | ✅ implemented | `handle_reset_parameters` (:387) → Parameters (T=1 defaults) | host tests, fuzz | resets to T=1 (contactless default), not spec T=0 default — deliberate for NFC |
| 6.1.7 | SetParameters | 0x61 | ⚠️ partial | `handle_set_parameters` (:382) → Parameters | battery (as "GetParameters (no card)" case, 0x61), host tests, fuzz | records `bProtocolNum` only (`header.specific[0]`); `abProtocolDataStructure` payload neither validated nor stored — response always echoes defaults (store-and-ack semantics) |
| 6.1.8 | Escape | 0x6B | ⚠️ partial (vendor) | `handle_escape` (:395) → RDR_to_PC_Escape | battery (0x02, 0x010101 cases), host tests, bench 0xD0/0xD1 | see Escape table below |
| 6.1.9 | IccClock | 0x6E | ⚠️ stub | catch-all (:115) → SlotStatus failed + CMD_NOT_SUPPORTED | fuzz (all-msg-types rounds) | NFC field clock not controllable; intentional |
| 6.1.10 | T0APDU | 0x6A | ⚠️ stub | catch-all | fuzz | TPDU-level control; intentional |
| 6.1.11/12 | Secure (PIN) | 0x69 | ⚠️ stub | catch-all | fuzz | no PIN entry hardware on ESP32 boards (contrast: STM32 Cherry profile implements it) |
| 6.1.12 | Mechanical | 0x71 | ⚠️ stub | catch-all | fuzz (0x71 round-trips), battery ("unknown command" class) | no mechanical hardware; intentional |
| 6.1.13 | Abort | 0x72 | ⚠️ stub | catch-all | fuzz | no USB control endpoint on serial transport; synchronous single-slot server has nothing to abort (matches STM32 audit's stub rationale) |
| 6.1.14 | SetDataRateAndClockFrequency | 0x73 | ⚠️ stub | catch-all | fuzz | baud fixed at 115200 by libccidtwin (see Serial Performance Notes in AGENTS.md) |

Truncated transport frames (declared `dwLength` exceeds delivered bytes) are
rejected before dispatch with a counted protocol error
(`ccid_handler.rs:88-102`); every failed response increments the diagnostics
error counter (`:126-128`).

### XfrBlock notes (§6.1.4)

Ordering of the local-answer logic (`ccid_handler.rs:256-338`):

1. **Inactive slot** → DataBlock, `bmCommandStatus=failed`, `bError=0x05`
   (`ICC_NOT_ACTIVE`, `ccid_types.rs:9`). Note: 0x05 is not a Table 6.2-2 code —
   a GemPC-lineage convention; tolerated by libccidtwin and the battery checks
   command-status bits only.
2. **PPS request** (`ccid_core::pps::is_pps_request`) → echoed verbatim, never
   forwarded to the card. This backs the `dwFeatures` bit 0x40 (automatic
   parameter negotiation) that keeps pcscd from sending PPS to a contactless
   card. Not counted in `apdu_tx/rx` diagnostics.
3. **PC/SC pseudo-APDU, CLA=0xFF, ≥ 4 header bytes** — answered by the reader
   itself, never forwarded (fix `c8a9782`, issues #49/#50):
   - `FF CA 00 00` (GET UID) → anticollision-cached UID + `9000`; no cached UID
     → `6300` (issue **#49**: GET UID was previously unsupported and forwarded
     to the card, which failed)
   - `FF CA` with bad P1/P2 → `6A86`
   - any other FF INS → `6300` (issue **#50**: reader answers `6300` matching
     the ACR1252 commercial-reader reference; the card itself would answer
     `6E00` class-not-supported — the deliberate 6E00-vs-6300 divergence)
   - short (< 4 byte) FF fragments are NOT routable and still flow to the card
4. **Everything else** (CLA=0x00, F0-class, …) → `nfc.transmit_apdu` relay;
   transport error → slot downgraded to PresentInactive + DataBlock failed +
   `HW_ERROR`.

Max payload 261 bytes enforced at the framing layer (`Overflow`); `bBWI` and
`wLevelParameter` ignored (synchronous single-slot server).

### Escape table (§6.1.8)

| Escape payload | Response payload | Purpose |
|---|---|---|
| `0x02` | `"GemPC Twin ESP32 1.0\0"` | firmware version — libccidtwin identification handshake; bench identity probe for the m5stick |
| `0x01 0x01 0x01` | echo `01 01 01` | enable synchronous card-movement notifications (libccidtwin handshake) |
| `0x1F 0x02` | empty | GemPC Twin host-handshake sequence, acknowledged |
| `0xD0` | 28-byte LE `Diagnostics` | vendor-neutral diagnostic counters (apdu tx/rx, NAK, errors, reinits, presence, uptime) — same wire format as the STM32 firmware |
| `0xD1` | echo `D1`, then panic | dump-and-retrieve: serve loop panics AFTER the ack is on the wire → flash coredump (AGENTS.md "Crash Dumps & Snapshot Debugging") |
| anything else | SlotStatus failed + `CMD_NOT_SUPPORTED` | |

Note: the **`0x6A` firmware-features escape (Gemalto IDBridge convention)
exists only on the STM32 firmware** (see `docs/CCID_SPEC_AUDIT.md` §6.1.8) —
esp32-ccid does not implement it. Host-side use of 0xD0 over USB requires
`ifdDriverOptions=0x0001` (`FEATURE_CCID_ESC_COMMAND`); over the serial wire it
is a direct exchange.

---

## RDR_to_PC Responses (§6.2) and Notifications (§6.3)

| § | Message | Type | Emitted by | Verification |
|---|---|---|---|---|
| 6.2.1 | DataBlock | 0x80 | IccPowerOn (ATR), XfrBlock (APDU response / pseudo-APDU / SW answers) | battery, host tests, fuzz, 13/13 on-target |
| 6.2.2 | SlotStatus | 0x81 | IccPowerOff, GetSlotStatus, all error/stub paths | battery, soaks, fuzz |
| 6.2.3 | Parameters | 0x82 | Get/Set/ResetParameters (T=0: 5 bytes, T=1: 7 bytes per Table 6.2-3) | host tests, fuzz |
| 6.2.4 | Escape | 0x83 | all handled escape payloads | battery, host tests, bench |
| 6.2.5 | DataRateAndClockFrequency | 0x84 | **never** (SetDataRate is a stub) | n/a |
| 6.3.1 | NotifySlotChange | 0x50 | in-stream 2-byte frame `50 <02\|03>` on presence transition, **UART mains only** (`build_slot_change_notification`); USB-CDC main never notifies (poll-based) | `ccid_serial_server` host tests, bench pcscd interop |
| 6.3.2 | HardwareError | 0x51 | **never** (no interrupt-IN on serial transport) | n/a |

Status registers (`build_bstatus`, `ccid_protocol::status`):
`bmICCStatus` 0x00/0x01/0x02 from `SlotState::{PresentActive, PresentInactive,
Absent}` (spec-correct); `bmCommandStatus` failed = 0x40-set bits. Error codes
actually emitted: `CMD_NOT_SUPPORTED (0x00)`, `HW_ERROR (0xFB)`, and the
non-standard `ICC_NOT_ACTIVE (0x05)` on XfrBlock vs inactive slot.

Class-specific control requests (§5.3 ABORT / GET_CLOCK_FREQUENCIES /
GET_DATA_RATES) are a USB-control-endpoint concept — **not applicable** on the
serial transport; the constant set in `ccid-protocol/src/types.rs:42-44` serves
the shared crate only.

---

## Wire Verification State

| Layer | Evidence | Result |
|---|---|---|
| Host unit tests, esp32-ccid (`cargo test --target x86_64-unknown-linux-gnu`, CI `esp32-host-test`) | dispatch matrix incl. pseudo-APDU cases (#49/#50), parameters, escapes, serial-server semantics | **100/100** (verified on this branch) |
| Host unit tests, ccid-transport-serial (CI `stm32-test`) | framing round-trips, LRC bit sweep, truncation, overflow, NAK | **25/25** |
| CCID-handler fuzz suite (#92 Part A, `ccid_fuzz.rs`, commit `c072b94`) | ~5,300 malformed-input rounds: arbitrary bytes, oversized dwLength claims, truncated headers, slot/seq passthrough, state-machine storms, post-garbage resync | **zero findings** |
| Wire conformance battery (`conformance_battery.py`, both bench readers, 2026-10-09) | 7-case card-absent command battery, structural nucula↔m5stick agreement | **7/7 both readers, AGREE** |
| Wire fuzz battery (same script) | 87 cases/reader (7 fixed + 80 seeded-random) + resync proof after each | **0 wedges** |
| pcscd restart + GetSlotStatus soaks (same script) | 10 restarts, 1000 status round-trips/reader | clean |
| On-target CCID tests, nucula `pn7160-ccid` over USB-CDC (`tests/test_ccid.py`, see `docs/nucula-campaign/RUNBOOK.md`) | full protocol battery on hardware | **13/13** (68 s, ai-legion) |
| pcscd/libccidtwin interop | readers enumerate (`GemPCTwin serial`, `Nucula CCID`), ATRs served | bench known-good 2026-10-09 (AGENTS.md matrix) |

Battery case list (7): Escape(0x02), Escape(0x01 01 01), GetSlotStatus,
IccPowerOn(no card), IccPowerOff, SetParameters — labeled "GetParameters" in
the script but sends **0x61** with an empty payload — and XfrBlock(no card).
GetParameters proper (0x6C) is covered by host unit tests and the fuzz
all-msg-types rounds.

---

## Known Divergences Summary

1. **#49 GET UID** — reader-answered `FF CA 00 00` from cached anticollision
   UID (was: unsupported/forwarded). Divergence class: PC/SC pseudo-APDUs are
   outside CCID §6.1.4; answering them in the reader is a commercial-reader
   convention (ACR1252 parity).
2. **#50 CLA 6E00-vs-6300** — unsupported FF INS answered `6300` by the reader
   (ACR1252 reference behavior) where the card would answer `6E00`.
3. **Escape set** — vendor escapes 0x02 / 0x01 01 01 / 0x1F 02 (GemPC Twin
   handshake), 0xD0 (diagnostics), 0xD1 (coredump snapshot) diverge from the
   plain spec's "escape is optional/vendor" model; 0x6A (Gemalto
   firmware-features) is STM32-only.
4. **SetParameters store-and-ack** — protocol number recorded, structure
   payload not validated/stored; defaults always echoed. Invisible to
   libccidtwin (it sets parameters once after ATR).
5. **GetParameters static** — defaults, not ATR-derived (NFC backend has fixed
   Fi/Di; ISO 14443-4 handles framing).
6. **bPowerSelect ignored** on IccPowerOn while `VOLTAGE_SUPPORT=0x07` is
   advertised.
7. **`bError=0x05` (ICC_NOT_ACTIVE)** on XfrBlock vs inactive slot — not a
   Table 6.2-2 code (GemPC-lineage convention).
8. **Slot byte echoed verbatim** — no runtime rejection of non-zero slots
   (single-slot descriptor, `MAX_SLOT_INDEX=0`); fuzz proves no wedge.
9. **TPDU-level `dwFeatures` claim (0x00010270)** — inherited GemPC Twin
   declaration with local PPS echo (bit 0x40); the handler is a transparent
   APDU relay. Host-visible behavior validated by battery + pcscd interop.
10. **Bench-known NFC gaps (not CCID-dispatch bugs)**: m5stick ISO-DEP
    activation of the bench P71 card fails (power_on → HW_ERROR path, AGENTS.md
    open gap); nucula presence stickiness (#88, edge-triggered discovery NTFs).

---

## Summary

| Category | Count | Status |
|---|---|---|
| PC_to_RDR implemented (full) | 5 | IccPowerOn, IccPowerOff, GetSlotStatus, XfrBlock, GetParameters |
| Partial | 2 | SetParameters (store-and-ack), Escape (vendor set) |
| Intentional stubs | 6 | IccClock, T0APDU, Secure, Mechanical, Abort, SetDataRate — all answer CMD_NOT_SUPPORTED per spec guidance for unsupported commands |
| RDR_to_PC emitted | 4 + 1 | DataBlock, SlotStatus, Parameters, Escape (+ in-stream NotifySlotChange on UART mains) |

The core command set pcscd/libccidtwin exercises is fully implemented and
verified at four layers (unit, fuzz, wire battery, on-target). All stubs match
the "reader without that hardware" pattern and answer per spec; the divergences
above are deliberate GemPC Twin / ACR1252 parity choices, each anchored to a
commit or issue.

## Changelog

| Date | Author | Changes |
|------|--------|---------|
| 2026-10-09 | #92 Part C | Initial esp32-ccid spec audit (dispatch, framing, responses, wire-verification state, divergences) |
