# PC/SC Host-Contract Audit — firmware vs pcsc-lite expectations

**Issue #141 deliverable.** Audit lane: 2026-10-14, branch
`sisyphus/141-pcsc-host-contract` (includes the #158 answer-or-silence
shape — timeout findings audit the CURRENT serial integrity code).

Sources (provenance in `reference/README.md`):

- **pcscd** — `reference/pcsc-lite/` quote-only copy @ `6cadd81`
  (v2.5.2): winscard.c, winscard_svc.c, readerfactory.c, eventhandler.c,
  prothandler.c, ifdwrapper.c, pcscd.h.in, PCSC/{reader.h, ifdhandler.h,
  pcsclite.h.in}
- **libccid / libccidtwin** — `reference/CCID/` submodule @ `1a0f75d`
  (1.7.1-8): ifdhandler.c, ccid_serial.c, commands.c, defs.h,
  MacOSX/reader.h, readers/*.txt
- **Device identities** — `reference/CCID/readers/{GemPCTwinSerial,
  CherrySmartTerminalST2XXX, Gemalto_IDBridge_CT30, Gemalto_IDBridge_K30}.txt`

The load-bearing structural fact for this whole audit: **for the serial
GemPC-Twin path, the host does not ask the device what it is.**
`set_ccid_descriptor()` synthesizes the entire CCID descriptor host-side
(ccid_serial.c:620-642) — `dwFeatures=0x00010230` (TPDU level, auto
clock/baud, NAD≠0), `bPINSupport=0x00`, `dwProtocols=T0|T1`,
`dwMaxCCIDMessageLength=271`, `dwMaxIFSD=254`, `readTimeout=3 s`,
`echo=true`. The v2 part 10 feature list is likewise computed host-side
from that descriptor (ifdhandler.c:1649-1728). Our serial firmware's
contract is therefore **behavioral**, not declarative: it must *act
like* the descriptor the host believes in.

For the **USB path (STM32)**, the firmware's own CCID descriptor IS the
advertisement: profiles `reference/CCID/readers/*.txt` (Cherry ST-2xxx:
`dwFeatures=0x000100BA`, **bPINSupport=0x03**; CT30/K30: `0x00010230`,
`bPINSupport=0x00`).

## 1. v2 part 10 features — advertised vs implemented

How a feature is discovered: application → `SCardControl(
CM_IOCTL_GET_FEATURE_REQUEST)` → pcscd `IFDHControl` → driver returns a
TLV list of `(tag, ioctl_code)` (ccid ifdhandler.c:1649-1728; table
vocabulary in `reference/pcsc-lite/src/PCSC/reader.h`). A feature absent
from the list is simply unavailable — applications must cope
(`SCARD_E_UNSUPPORTED_FEATURE`); nothing degrades the reader itself.

| Feature | Serial GemPC-Twin (host-synthesized) | USB Cherry ST-2xxx (our descriptor) | USB CT30/K30 (our descriptor) | Firmware obligation |
|---|---|---|---|---|
| `FEATURE_VERIFY_PIN_DIRECT` | **not advertised** (bPINSupport=0) | **advertised** (bPINSupport bit0) | not advertised | USB-Cherry: must implement `PC_to_RDR_Secure` verify or applications fail at PIN time (see §1.1) |
| `FEATURE_MODIFY_PIN_DIRECT` | not advertised | **advertised** (bit1) | not advertised | same |
| `FEATURE_IFD_PIN_PROPERTIES` | not advertised | **advertised** (any bPINSupport≠0, ifdhandler.c:1683-1693) | not advertised | USB-Cherry: answer `IOCTL_FEATURE_IFD_PIN_PROPERTIES` (host does this itself — no device traffic) |
| `FEATURE_MCT_READER_DIRECT` | never (KOBIL only) | no | no | none |
| `FEATURE_GET_TLV_PROPERTIES` | **always advertised** (ifdhandler.c:1707-1712, unconditional) | always | always | see §1.2 — triggers a *device-visible* escape probe on Gemalto-vendor readers |
| `FEATURE_CCID_ESC_COMMAND` | advertised **iff** `ifdDriverOptions 0x0001` (ifdhandler.c:1715-1724; AGENTS.md documents the bench wiring) | same | same | our Escape 0x02/0xD0/0xD1/0xD2/0xD3 subcommands ride this |
| `SCARD_CTL_CODE(3400)` RDP alias | host handles | host | host | none |

### 1.1 PIN (VERIFY/MODIFY) — USB Cherry profile only

The PIN_VERIFY_STRUCTURE/PIN_MODIFY_STRUCTURE the host marshals are in
`reference/CCID/MacOSX/reader.h` (already the `PCSC` specquote source).
When `bPINSupport≠0`, the driver also answers
`IOCTL_FEATURE_VERIFY_PIN_DIRECT` by building `PC_to_RDR_Secure` and
sending it **to the device** (ifdhandler.c:1964+), with a **≥90 s read
window** (commands.c:508/896 `max(90, bTimeOut+10)` s). A Cherry-profile
firmware that does not implement `PC_to_RDR_Secure` fails every PIN
operation with a timeout/comm error rather than a clean
`CMD_NOT_SUPPORTED` — see findings F-3.

### 1.2 GET_TLV_PROPERTIES — the hidden firmware dependency

`IOCTL_FEATURE_GET_TLV_PROPERTIES` (ifdhandler.c:1766+) is answered from
the descriptor, **but** for `VENDOR_GEMALTO` readers the driver also
sends Escape `{0x02}` to the device to fetch `sFirmwareID`
(ifdhandler.c:1806-1824). GEMPCTWIN **is** Gemalto-vendor, so any
`SCardControl(GET_TLV_PROPERTIES)` (pcsc_scan does exactly this) puts a
`PC_to_RDR_Escape(abData=02)` on our serial wire. Tolerance: failure is
swallowed (`if (IFD_SUCCESS == ret)`), the property is simply omitted —
but the escape consumes a bSeq and the driver's read window applies.

### 1.3 ICC status via ESCAPE

Issue asks about "ICC status via ESCAPE": there is **no
`FEATURE_ICC_STATUS`-style device query in this driver** — the
SCARD_ATTR_ICC_PRESENCE path (§2) is how presence is fetched, driven by
pcscd's polling thread. No firmware obligation beyond correct
GetSlotStatus/notify semantics.

## 2. SCARD_ATTR_* — what pcscd queries, must-answer vs may-refuse

`SCardGetAttrib` → pcscd `IFDGetCapabilities` (winscard.c:1401) → driver
`IFDHGetCapabilities` (ccid ifdhandler.c:420-705). Refusal is **legal
and routine**: unknown tag → `IFD_ERROR_TAG` →
`SCARD_E_UNSUPPORTED_FEATURE` (winscard.c:1407-1426). The **only**
attributes pcscd itself synthesizes when the driver refuses are
`SCARD_ATTR_DEVICE_FRIENDLY_NAME` and `SCARD_ATTR_DEVICE_SYSTEM_NAME`
(winscard.c:1408-1424). Nothing at reader startup blocks on attributes:
readerfactory.c queries only `TAG_IFD_THREAD_SAFE` (:355),
`TAG_IFD_POLLING_THREAD_WITH_TIMEOUT` (:411), `TAG_IFD_SLOTS_NUMBER`
(:440), `TAG_IFD_SIMULTANEOUS_ACCESS` (:765) — all answered by the
driver from its own data, no device I/O.

| Attribute | Driver answers? | Backed by device traffic? | Serial-twin value (host-side) | Our firmware obligation |
|---|---|---|---|---|
| `SCARD_ATTR_ATR_STRING` (`TAG_IFD_ATR`) | yes (ifdhandler.c:437-450) | no — driver caches ATR from PowerICC | cached ATR | serve correct ATR bytes in `RDR_to_PC_DataBlock` on IccPowerOn; max 33 B (`MAX_ATR_SIZE`) |
| `SCARD_ATTR_ICC_PRESENCE` | yes (:462-477) | **yes** — calls IFDHICCPresence live | 0 or **2** (present; ccid uses 2, not Windows' 1/2/4 convention) | correct bmICCSlotStatus in GetSlotStatus + NotifySlotChange cadence |
| `SCARD_ATTR_ICC_INTERFACE_STATUS` | yes (:452-460) | yes (same poll) | 0/1 | same |
| `SCARD_ATTR_MAXINPUT` | yes (:564-568) | no | 271−10 = 261 | frame bodies must respect dwMaxCCIDMessageLength=271 on this profile |
| `SCARD_ATTR_VENDOR_NAME` | yes (:547-562) | no | sIFD_iManufacturer — for TWIN builds: absent → len 0 | none (empty is legal) |
| `SCARD_ATTR_VENDOR_IFD_SERIAL_NO` | yes (:669-685) | no | for TWIN: absent → len 0 | none |
| `SCARD_ATTR_VENDOR_IFD_VERSION` | yes (:534-545) | no | IFD_bcdDevice<<16 | none (USB: matches our bcdDevice) |
| `SCARD_ATTR_CHANNEL_ID` | yes, USB only (:688-698, `#if !TWIN_SERIAL`) | no | n/a | none |
| `SCARD_ATTR_DEVICE_FRIENDLY_NAME` / `_SYSTEM_NAME` | driver may refuse; **pcscd answers** | no | reader.conf FRIENDLYNAME | none (enrollment naming, §4) |
| everything else (`CURRENT_PROTOCOL`, `PROTOCOL_*`, `CHARACTERISTICS`, clock/F/D tables, …) | refused → `SCARD_E_UNSUPPORTED_FEATURE` | — | — | **may refuse; no obligation** |

Key asymmetry vs the issue's assumption: pcscd does **not** probe
attributes at startup and never blocks on them; the only
device-traffic-backed attributes are the two presence codes, which ride
the 400 ms poll (§3).

## 3. Timeout semantics — what pcscd considers an error vs forever-block

### 3.1 Host-side patience budget (no SCardTransmit timeout exists)

`SCardTransmit` passes **no timeout** to the driver (winscard.c:1491-1653
— `SCARD_IO_REQUEST` carries protocol+length only). pcscd's worker
thread blocks in the IFD call for as long as the driver does; the
driver's patience is `readTimeout` (ccid_serial.c:390-404, `select()`
per ReadChunk). The serial-twin budget per command class:

| Exchange | Host read window | Citation |
|---|---|---|
| GetSlotStatus (presence poll, every 400 ms) | **3 s** (`DEFAULT_COM_READ_TIMEOUT`, defs.h:76; forced at ifdhandler.c IFDHICCPresence) | defs.h:76 |
| IccPowerOn/Reset (ATR wait) | **60 s** (ifdhandler.c:1336-1373: "card can use up to (9600+12)*33 ETU… 4 MHz ⇒ 29 s", budget 60 s) | ifdhandler.c:1365-1373 |
| IccPowerOff | 3 s default (no bump in the IFD_POWER_DOWN path) | ifdhandler.c:1317-1338 |
| XfrBlock TPDU | computed from card params: T=0 `261·EGT+6·WWT` (ifdhandler.c:2409-2454), T=1 BWT-based with **WTX multiplier** (`readTimeout *= wtx`, commands.c:573); 60 s floor for non-ISO params (:2439) | ifdhandler.c:1119-1203 |
| Escape (FEATURE_CCID_ESC_COMMAND) | **30 s** ("30 seconds timeout for long commands", ifdhandler.c:1637-1639) | ifdhandler.c:1588 |
| Gemalto firmware-ID probe escape | special 1 s (ccid_serial.c:790-796) / driver-managed | ccid_serial.c:790 |
| PIN verify/modify | **≥90 s** (`max(90, bTimeOut+10)`, commands.c:508/896) | commands.c:508 |

A missed window = `STATUS_COMM_ERROR` from ReadSerial → the exchange
fails at the pcscd layer (SCARD_E_NOT_TRANSACTED / reader error state).
This is the budget #158's `RESPONSE_DEADLINE_TICKS = 2500 ms` exists to
respect (3 s minus wire/host margin, refreshed by CCID time-extension
frames).

### 3.2 pcscd daemon-side cadence (what the firmware sees unprompted)

- **Presence poll**: no `pthCardEvent` for TWIN_SERIAL
  (`TAG_IFD_POLLING_THREAD_WITH_TIMEOUT` is `#if !defined(TWIN_SERIAL)`,
  ifdhandler.c:570-610) → pcscd's generic loop polls `IFDHICCPresence`
  every **400 ms** (`PCSCLITE_STATUS_POLL_RATE`, pcscd.h.in:53;
  eventhandler.c:465-481) — each poll is a serial GetSlotStatus with the
  3 s window. The firmware must tolerate this cadence indefinitely.
- **Auto power-off**: a card left powered but unused is powered down
  after the **5 s grace period**
  (`PCSCLITE_POWER_OFF_GRACE_PERIOD`, pcscd.h.in:70; eventhandler.c:476,
  490-506) — unsolicited-from-firmware's-view `PC_to_RDR_IccPowerOff`
  MUST be served promptly and correctly.
- **Reader-open probe**: `CreateChannelByNameOrChannel` issues up to
  **two** GetSlotStatus reads at channel open (ifdhandler.c:166-183) —
  the handshake our boot banner must not collide with (clean-wire
  doctrine, AGENTS.md).
- **Protocol negotiation**: every SCardConnect(T=1) drives
  `IFDHSetProtocolParameters` → SetParameters + PPS attempt
  (prothandler.c:60-160: T=1 preferred, fallback T=0, never both-retry).
  The #101 lesson (SetParameters arms the t1 route) lives here.

### 3.3 What forever-blocks vs errors

- A transmit that never answers: the pcscd worker thread blocks inside
  the driver until `readTimeout` fires → comm error surfaces to the
  client. pcscd has **no daemon-wide watchdog** on transmits.
- The presence poll timing out is not fatal by itself (reader stays
  enumerated; status churns) — but pcscd restarts (the #153 flap class)
  are host-side reactions to persistent comm errors.
- Serial stream corruption **is** fatal-per-session: ReadSerial treats
  any unexpected first byte as `STATUS_COMM_ERROR` (ccid_serial.c:194-201,
  the "Got 0x73" journal signature); there is no mid-stream resync. Slot
  notifies may interleave only at frame starts (ccid_serial.c:188-221).

## 4. Reader naming / iSerial conventions (multi-reader benches)

- Serial enrollment: `/etc/reader.conf.d/*` — `FRIENDLYNAME
  "GemPC Twin"`, DEVICENAME (no colons in the path — the parser splits
  on the first), LIBPATH to `ifd-ccid.bundle`, `CHANNELID 1`; the
  FRIENDLYNAME string is what selects the synthesized descriptor
  (ccid_serial.c set_ccid_descriptor matches reader_name; default
  GEMPCTWIN). Reference: `reference/pcsc-lite/doc/reader.conf.5.in`,
  AGENTS.md "pcscd reader.conf trap".
- USB: pcscd appends `(iSerial)` to disambiguate same-VID:PID readers —
  the bench doctrine (`ST2XXX-001` vs `ST2XXX-F746`, AGENTS.md) and the
  labgrid DUTs-disjoint-by-identity rule. `SCARD_ATTR_VENDOR_IFD_SERIAL_NO`
  is a *different* thing (IFD-level string, absent for the twin).

## 5. Findings (severity-ranked)

Verified against the tree @ `sisyphus/141-pcsc-host-contract` (base: main +
#158 `20ce9b2`). Firmware surface: `crates/ccid-core/src/handler.rs`
(shared dispatcher, all ESP32 transports), `firmware/ccid-firmware/src/
ccid_core.rs` (STM32 USB wrapper), `firmware/esp32-ccid/src/ccid_handler.rs`
(T=1 route + profile escapes), serve loops in
`firmware/esp32-ccid/src/{main,ccid_serial_server,pn7160_ccid,wifi_ccid,
spp_server}.rs`.

### F-1 — Unknown-escape answers were SlotStatus-shaped on every ESP32 transport — FIXED

- **Severity**: Medium (conformance + sibling divergence; driver-tolerated)
- **Was**: `crates/ccid-core/src/handler.rs` `handle_escape` fallback answered
  unsupported escape payloads with a `RDR_to_PC_SlotStatus` frame
  (FAILED + `CMD_NOT_SUPPORTED`).
- **Reference**: osmo-ccid-firmware answers failed escapes with an
  **Escape-typed** frame — `ccid_device.c:585`:
  `resp = ccid_gen_escape(cs, seq, CCID_CMD_STATUS_FAILED, CCID_ERR_CMD_NOT_SUPPORTED, NULL, 0);`
  — and our own STM32 sibling does the same (`send_err_resp`,
  `ccid_core.rs:296-311` → `RDR_TO_PC_ESCAPE`). CCID Rev 1.1 §6.1.8 maps
  PC_to_RDR_Escape to RDR_to_PC_Escape. The repo's own
  `tests/ccid_core_tests.rs:617 test_error_response_type_for_escape` already
  locked the Escape shape for the STM32 path — the ESP32 core was the lone
  divergent sibling (fix-all-siblings protocol).
- **Host impact**: none today — `CmdEscapeCheck` (commands.c:963-1092) never
  checks the response type, only length + `CCID_COMMAND_FAILED` bit — but the
  divergence is exactly the mock/hardware-divergence bug class.
- **Fix**: fallback + `0x1F 0x02` arm now emit `RDR_TO_PC_ESCAPE` FAILED +
  `CMD_NOT_SUPPORTED`; regression tests
  `test_escape_unknown_payload_answers_escape_type`,
  `escape_0xd2_only_matches_single_byte_payload` updated, battery untouched
  (it never probes unknown escapes).

### F-2 — TPDU→APDU switch escape (`0x1F 0x02`) was a silent success no-op — FIXED

- **Severity**: Medium (latent; triggered only by non-default host config)
- **Was**: `handle_escape` answered `[0x1F, 0x02]` with success + empty
  payload without changing any behavior.
- **Reference**: libccid `ccid_open_hack_post()` sends exactly this escape
  for GEMPCKEY/GEMPCTWIN when `DriverOptions &
  DRIVER_OPTION_GEMPC_TWIN_KEY_APDU` and — **on success** — rewrites the
  synthesized `dwFeatures` to `CCID_CLASS_SHORT_APDU`
  (reference/CCID/src/ccid.c:299-313). Our firmware speaks TPDU level only
  (T=1 firmware-terminated, AGENTS.md doctrine), so a success ack tells the
  host a framing lie: pcscd would then send raw APDUs where the firmware
  expects T=1 blocks. `libccidtwin` never sends `0x1F` on its own (grep:
  only the ccid.c USB hack + one `{0x1F,0x01}` special case in commands.c),
  so the default bench config never trips it.
- **Fix**: the arm now answers Escape FAILED + `CMD_NOT_SUPPORTED` — the
  driver keeps TPDU level, which is what we speak.

### F-3 — STM32 `0x6A` firmware-features answer had the wrong size — FIXED

- **Severity**: Low-Medium (USB CT30/K30 profiles; feature advertised in
  docs but dead on arrival at the driver)
- **Was**: `firmware/ccid-firmware/src/ccid_core.rs` answered the Gemalto
  GET_FIRMWARE_FEATURES escape with a 15-byte `[1,0,…]` "TLV-style bitmap"
  (test even mislabeled `data[0]=1` as `bNumberMessageFix`).
- **Reference**: libccid accepts the answer **only at exactly
  `sizeof(struct GEMALTO_FIRMWARE_FEATURES)`** — ccid.c:265-266
  `if ((IFD_SUCCESS == ret) && (len_features == sizeof *gf_features))` —
  any other length is silently discarded. The struct
  (reference/CCID/src/ccid.h:228-281) is 21 bytes: 4 LCD fields, 3 bitfield
  bytes, `RFUb2[2]`, `RFUb4[3]`, `VersionNumber`, `MinimumPINSize`,
  `MaximumPINSize`, Firewall byte, `FirewalledCommand_SW1/SW2`, `RFUb6[3]`.
- **Fix**: 21-byte layout per ccid.h (no LCD, no PIN features, VersionNumber
  = 1 — consistent with `bPINSupport=0x00`/`wLcdLayout=0x0000` in the CT30/
  K30 profiles), quote-locked via the new `CCID_DEFS` specquotes source;
  tests updated (`data_len == 21`, `data[12] == 1`).

### F-4 — `late_drop_count` not surfaced in the Escape 0xD0 layout — deferred

- **Severity**: Info. `ccid_serial_server.rs` counts late drops (#153) but
  the 28-byte 0xD0 golden layout deliberately does not carry it — the
  comment parks this on #156 A3's layout decision. Cross-lane handoff only;
  no change in this audit (do not fork the golden layout).

### F-5 — Escape `0x02` firmware-ID: VERIFIED CORRECT (positive finding)

- pcscd's `GET_TLV_PROPERTIES` handler probes Gemalto-vendor readers with
  `Escape{0x02}` for `sFirmwareID` (ifdhandler.c:1806-1824). Two facts make
  this work for us end-to-end: (a) `CmdEscapeCheck` treats `timeout == 0` as
  "keep the default 3 s window" (commands.c:977-982) — not a zero wait, so
  the probe really reads our answer; (b) our serial handler answers
  `b"GemPC Twin ESP32 1.0\0"` (handler.rs, bench-proven on the m5stick).
  Result: `PCSCv2_PART10_PROPERTY_sFirmwareID` is populated from our string
  through stock pcscd + libccidtwin. No firmware obligation exists beyond
  keeping that escape fast (<3 s) and well-formed.

### F-6 — Attributes: firmware owes NOTHING beyond ATR + presence — VERIFIED

- `SCardGetAttrib` refusal is legal (`IFD_ERROR_TAG` →
  `SCARD_E_UNSUPPORTED_FEATURE`, winscard.c:1401-1426); pcscd answers
  FRIENDLY/SYSTEM names itself; the driver answers the rest from
  host-side data with **no device I/O except the two presence codes**
  (§2). The firmware's obligations are exactly: correct ATR bytes in the
  PowerOn DataBlock (≤33 B) and truthful bmICCSlotStatus — both
  bench-proven. `SCARD_ATTR_ICC_PRESENCE=2`-when-present is a ccid-side
  convention (ifdhandler.c:462-477), nothing for us to emit.

### F-7 — Timeouts post-#158: within every host window — VERIFIED

- Per-exchange budget check (§3.1) against the current shape:
  - `RESPONSE_DEADLINE_TICKS = 2500 ms` < the 3 s `ReadChunk` select window
    with 500 ms margin (ccid_serial_server.rs, cites defs.h:76) ✓
  - WTX rounds: firmware answers S(WTX request) after ≈BWT (1.43 s @
    BWI=4/10752 baud) + 100 ms margin — each serial exchange completes
    well under 2.5 s, and each host→firmware round re-arms the window ✓
    (t1.rs:340-395)
  - PowerOn: 60 s host window vs ms-scale NFC activations (4×retry ≈
    worst case seconds) ✓; PowerOff answered in-line ✓
  - The unsolicited 5 s-grace auto power-off (eventhandler.c) and the
    400 ms presence cadence are handled by the serve loops' poll intervals
    (3 s UART / 500 ms CDC) — GetSlotStatus answers are immediate ✓
  - `refresh_host_deadline()` is wired but dormant: the serial path never
    emits CCID-level time-extension status (T=1 WTX is done in-band), so
    the hook is future capacity, not a gap.
- Open cross-lane item: PN7160 exchange budget (2.4 s) lives in
  `crates/pn7160-nci` — #156 lane owns it; audit-only here.

### F-8 — v2p10 feature vocabulary corrections (issue text vs reality)

- There is **no `FEATURE_ICC_STATUS`** in pcsc-lite's reader.h (the 25-code
  table tops out at `FEATURE_CCID_ESC_COMMAND 0x13` / `EXECUTE_PACE 0x20`);
  presence detection is the pcscd poll (§3.2), not a part 10 feature.
- There is **no `PCSCv2_PART10_PROPERTY_wIdr`** — the reader-identity
  properties are `wIdVendor` (11) / `wIdProduct` (12), consumed by OpenSC
  (reader-pcsc.c). Both are driver-side derived (USB VID:PID; host-synth
  readerID for serial) — again no firmware surface.
- `FEATURE_GET_TLV_PROPERTIES` is **always advertised** by the ccid driver
  (ifdhandler.c:1707-1712) regardless of device — the "should we?" question
  is answered host-side: we cannot opt out and need not act, except the
  one device-visible consequence (F-5's `0x02` probe).

## 6. Cross-references

- Serial integrity shape (answer-or-silence, deadline refresh): PR #158
  / issue #153.
- WTX / NCI-side timeouts: #156 lane (`crates/pn7160-nci`, audit-only
  here).
- Bench pcsc_scan snapshots: acceptance artifact, hardware lane.
