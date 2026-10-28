# Transport host drivers — libccidtwin vs USB-CCID (libccid) vs VPCD (libifdvpcd)

Date: 2026-10-21 · Branch: `docs/transport-drivers`
Scope: how the three host-side driver stacks we serve actually work, their
tradeoffs, the limitations we have hit on the bench (each with its issue),
and an opportunities scan of what other projects do — what to adopt, and
where twin emulation could be replaced by a cleaner path.

Sources: in-tree pins `reference/CCID/` @ `1a0f75d` (libccid 1.7.1-8),
`reference/pcsc-lite/` @ `6cadd81` (v2.5.2), `reference/osmo-ccid-firmware/`
@ `639db14` (provenance: `reference/README.md`); upstream URLs cited inline.
Analysis calls are marked **[ours]** where they are interpretation, not a
quote.

Our three transport lanes (AGENTS.md):

| Lane | Wire | Host driver | Reader in pcscd |
|---|---|---|---|
| STM32 (F469/F746) | native USB CCID class | `libccid.so` (ifd-ccid bundle) | auto, hotplug |
| ESP32 m5stick | GemPC Twin serial framing, UART0 115200 8N2 via FTDI bridge | `libccidtwin.so` | `GemPCTwin serial 00 00` (reader.conf) |
| ESP32-C3 nucula | GemPC framing over USB-CDC **via host PTY broker**; or vpcd protocol over TCP (wifi) | `libccidtwin.so` on the PTY / stock `libifdvpcd.so` (bench-patched, #359) | `Nucula CCID 00 00` / `Nucula VPCD 00 00` |

---

## TL;DR

- **USB CCID class is the only zero-config, hotplug, PIN-pad-capable,
  driverless-everywhere path.** Everything else we run is a workaround for
  silicon that lacks a USB device controller. Industry agrees: every modern
  reader/key (YubiKey, Nitrokey, ELATEC, Identiv, sysmoOCTSIM) presents
  CCID over USB; nobody else emulates a 1998 GemPC Twin serial protocol.
- **libccidtwin works but is frozen**: B115200 hardcode (#54), strict
  no-resync parser (#131), no hotplug (config-file reader), host-synthesized
  descriptor that makes PIN pads impossible on this lane. Keep it for the
  m5stick, harden it, never fork it locally (rejected once, #54).
- **VPCD is the right network lane** (protocol-transparent, stock driver)
  but the stock `libifdvpcd` carries real bugs we had to patch (#359) and it
  has no hotplug/eject-free presence story. Upstream our patches.
- **The C3 reset wall (#117) is silicon**, not software: twin-over-CDC needs
  the broker forever on that board. C6 (`CHIP_RST_DIS` bit) and S2/S3
  (real USB-OTG + `usbd-ccid`/TinyUSB app-class hook) remove the wall for
  the next board rev.

---

## 1. How each host driver actually works

### 1.1 libccid — the USB CCID class driver

- **Discovery/hotplug**: none needed by the user. pcscd's hotplug machinery
  auto-loads the ifd-ccid bundle for USB devices; the reader.conf(5) man
  page is explicit: *"USB readers SHALL NOT be configured using these files.
  pcscd uses another mechanism to automatically load USB drivers"*
  ([reader.conf(5)](https://manpages.opensuse.org/Tumbleweed/pcsc-lite/reader.conf.5.en.html)).
  Matching is VID:PID against the bundle's `Info.plist`
  (`ifdVendorID`/`ifdProductID`, in-tree `reference/CCID/`); the supported
  list is published at [ccid.apdu.fr/ccid/supported.html](https://ccid.apdu.fr/ccid/supported.html).
  Our STM32 firmware therefore advertises one of three reference commercial
  identities (Cherry SmartTerminal ST-2xxx, Gemalto IDBridge CT30/K30 —
  `reference/CCID/readers/*.txt`) so stock hosts recognise it driverless.
- **Framing**: USB CCID Rev 1.1 messages — 10-byte CCID header + payload on
  bulk endpoints, plus an interrupt-IN endpoint for
  `RDR_TO_PC_NOTIFY_SLOT_CHANGE` slot events
  ([USB CCID 1.1 spec](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf)).
  Time-extension (`bTimeExtension`) lets a slow card hold the host's hand
  off without a timeout — no equivalent exists on the serial lane.
- **Ownership of T=1**: determined by the *device's* CCID descriptor
  (`dwFeatures` exchange-level bits): character / TPDU / short-APDU /
  extended-APDU. At TPDU level the host-side T=1 engine inside libccid
  (`src/openct/proto-t1.c`, in-tree) drives the protocol; at APDU level the
  device owns it. Our USB profiles are TPDU level (`0x000100BA` Cherry,
  `0x00010230` CT30/K30 — `docs/PCSC_HOST_CONTRACT_AUDIT.md`).
- **Reset/reconnect semantics**: `IccPowerOff`/`IccPowerOn` per
  SCardConnect (cold reset), Abort pipe for sequence recovery, and the
  descriptor's auto-configuration bits decide clocks/PPS. Reader unplug =
  hotplug remove; reader replug = hotplug add — no pcscd restart ever.
- **The contract is declarative**: what the descriptor says is what the
  host believes (v2 part 10 feature list computed from the descriptor,
  `ifdhandler.c:1649-1728`, in-tree).

### 1.2 libccidtwin — serial GemPC Twin emulation

- **Discovery**: a hand-written `/etc/reader.conf.d/` entry
  (`FRIENDLYNAME`/`DEVICENAME`/`LIBPATH`/`CHANNELID`,
  [reader.conf(5)](https://manpages.opensuse.org/Tumbleweed/pcsc-lite/reader.conf.5.en.html)).
  Serial readers are opened **once at pcscd start** by
  `RFStartSerialReaders` (`reference/pcsc-lite/src/readerfactory.c:1484`);
  re-read happens only when the config file changes (`RFReCheckReaderConf`).
  **No hotplug.** If the reader is mid-boot when pcscd starts, init fails
  (`Get firmware failed`) and a pcscd restart is needed — bench-proven
  (#101). The driver is built from the same CCID source with
  `--enable-twinserial`
  ([CCID README history](https://github.com/LudovicRousseau/CCID/blob/master/README.md)).
- **Open sequence / line control**: `open()` → `TIOCMSET` clearing RTS
  (Gemplus "plug-and-play inhibition") → `tcsetattr` — and this is the
  DTR/RTS chatter that chip-resets an ESP32-C3 on *every* open (#117,
  `docs/NOCULA_USB.md`).   Speed is **hardcoded**
  `cfsetspeed(&current_termios, B115200)` (`reference/CCID/src/ccid_serial.c:860-863`).
  The only speed negotiation upstream knows is the GemCore SIMPro2 probe:
  start at 9600, `CmdEscape {0x01, 0x10, 0x20}` → switch to 115200
  (`ccid_serial.c:840-858`) — the pattern a baud-knob patch must follow (#54),
  and the pattern our firmware-side `[0x05, code]` escape implements
  (see §3.1; inert against stock pcscd).
- **Framing**: GemPC serial framing — `SYNC 0x03` + CTRL (`ACK 0x06` /
  `NAK 0x15`) + a full CCID message (10-byte header + data) + XOR LRC;
  every transmitted byte is **echoed** by the reader; max CCID message 271 B
  (our `crates/ccid-transport-serial`, mirroring `ccid_serial.c` comments).
  The parser is a **strict byte stream with NO resync**: one stray byte
  desyncs pcscd for the whole session — the root of the clean-wire doctrine
  (#131, AGENTS.md "Clean-wire doctrine (inviolable)").
- **Ownership of T=1**: host-side (libccidtwin runs the same openct T=1
  engine; the synthesized descriptor is TPDU level). The *reader* answers
  S-blocks locally and relays I-block INF — which is exactly what our
  `t1.rs` endpoint does and what the real Twin does (AGENTS.md "Role
  doctrine" §3, `docs/T1_BEST_PRACTICES.md`).
- **Reset/reconnect semantics**: pcscd **keeps the card powered between
  client connections**; libccidtwin resets its T=1 counters per
  `SCardConnect`. Our firmware must reset its endpoint state at the same
  anchor — the #330/#345 lesson: stale counters/chains survived the
  `SetParameters(T=1)` re-arm and every fresh connection's ns=0 I-blocks
  hit stale state (R-block churn → `6A88`/`6E00`). Fixed in PR #345 by
  resetting the T=1 endpoint on `SetParameters(T=1)`.
- **The contract is behavioral, not declarative** — the load-bearing fact:
  `set_ccid_descriptor()` synthesizes the entire CCID descriptor
  *host-side* (`ccid_serial.c:620-642`): `dwFeatures=0x00010230`,
  `bPINSupport=0x00`, `dwMaxCCIDMessageLength=271`, `dwMaxIFSD=254`,
  `readTimeout=3 s`, `echo=true`. *"For the serial GemPC-Twin path, the
  host does not ask the device what it is"* (`docs/PCSC_HOST_CONTRACT_AUDIT.md`).
  Consequence: **no descriptor trick can ever change what the host believes
  on this lane** — including PIN-pad support (§2).

### 1.3 libifdvpcd — the vsmartcard VPCD driver

- **Discovery**: also a reader.conf.d entry
  (`LIBPATH /usr/lib/pcsc/drivers/serial/libifdvpcd.so`), but the mode
  comes from `DEVICENAME`/`CHANNELID`: by default the **driver listens**
  (server mode) on ports 35963 (+1 per slot) and the *card side* connects
  in; if `DEVICENAME` is a hostname the driver **connects out** (client
  mode) and the card side must run `--reversed`
  ([vsmartcard README](https://github.com/frankmorgner/vsmartcard/blob/master/virtualsmartcard/doc/README.txt.in)).
  `vpcd-config` generates the config (and a QR code) from the local IP
  ([vpcd-config.c](https://github.com/frankmorgner/vsmartcard/blob/7c949d1e/virtualsmartcard/src/vpcd-config/vpcd-config.c)).
  **No hotplug** — the reader exists when pcscd starts; presence of the
  *card* is a poll (`IFDHICCPresence` → ATR probe, `docs/VPCD_ALIGNMENT_AUDIT.md`).
- **Framing**: minimal — every message is a 2-byte big-endian length prefix
  + payload. Control payloads are exactly 1 byte (`ATR`/`ON`/`OFF`/`RESET`);
  an APDU message is the full C-APDU, answered with the full R-APDU
  (`vsmartcard/src/vpcd/vpcd.c`, alignment table in
  `docs/VPCD_ALIGNMENT_AUDIT.md`). Protocol supports 64-KiB extended APDUs
  (our NFC path is 261 B — ISO-DEP's limit, not vpcd's).
- **Ownership of T=1**: **none — there is no T=1 on the wire.** The vpicc
  *is* the card; the driver hands pcscd whole APDUs and whole answers. The
  pcscd protocol layer (SetParameters, T=0/T=1 negotiation) is bypassed.
  This is the lane where a host↔card secure channel is trivially
  transparent **[ours]**.
- **Reset/reconnect semantics**: `CTRL_ON`/`OFF`/`RESET` map to power
  on/off/warm reset; a zero-length ATR answer means "ICC absent" and makes
  the driver **eject** the connection and re-poll (`vicc_eject`). Error
  handling is eject-and-reconnect — robust *by design*, but see §3: the
  stock driver had two bugs that turned eject into wedge (#359, #326).
- **Bench specifics**: client mode crashes pcscd with a glibc `FD_SETSIZE`
  abort on our host, so we run the stock **server** mode and the firmware
  connects out (reversed, auto-reconnect every 3 s) —
  `docs/VPCD_WIFI_RUNBOOK.md`.

### 1.4 The pcscd frame around all three

pcscd polls each reader through an `EHStatusHandlerThread` at
`PCSCLITE_STATUS_POLL_RATE` = 400 ms (`reference/pcsc-lite/src/pcscd.h.in:53`,
`eventhandler.c`) — the cadence behind slot-change detection on all lanes,
and the thread whose name appears in every twin desync error-loop (#101,
#131). A driver that wedges this thread (unbounded `recv()` in stock
libifdvpcd, #359) freezes that reader — and on our bench, starved sibling
sessions — until pcscd restart.

---

## 2. Tradeoff table

Measured numbers are bench numbers from the issues cited; wire-rate math is
from #51/#54 (115200 8N2 ≈ 87 µs/byte).

| Axis | libccidtwin (serial twin) | libccid (USB CCID class) | libifdvpcd (VPCD) |
|---|---|---|---|
| Transport latency overhead | ~2.6 ms serial per 30-B round trip (#54); ~5–6 ms wire incl. echo (#51 closure); FTDI `latency_timer=4` adds up to ~4 ms (must be 1, AGENTS.md); broker variant +0.5–0.72 ms PTY hop, but CDC serve loop costs ~300 ms/cmd today (#119) | Bulk transfers on 12 Mbps FS; bench proxy ACR1252 (USB CCID) = ~2 ms/APDU vs our 16 ms (#51) | Wifi RTT floor ~7 ms (`docs/transport-research-154.md`); firmware fire-and-collect budget → ~2 s/APDU today, early-return pending (runbook) |
| Baud / bandwidth ceiling | **B115200 hardcoded** host-side (#54); wire itself good to 3 Mbps (FTDI) / 5 Mbps (ESP32 UART) — the host is the constraint | USB FS bulk; extended APDUs if descriptor declares | TCP — protocol carries 64-KiB APDUs; wire never the limit |
| Hotplug robustness | None: config-file reader, opened at pcscd start (`readerfactory.c:1484`); boot race needs pcscd restart (#101); USB-bridge re-enumeration renumbers ports + resets `latency_timer` | Native: auto add/remove, replug just works (reader.conf(5)) | None as a reader; TCP half-open after board reset wedges the single-accept until pcscd restart (runbook §Recovery) |
| Out-of-the-box availability | ccid package ships `libccidtwin.so` **but** requires a hand-written reader.conf entry + pcscd restart | **Zero config**, driverless on Linux/macOS/Windows (Yubico ships on the base Windows driver, [yubico.com](https://www.yubico.com/authentication-standards/smart-card/)) | Separate vsmartcard package + reader.conf; on our bench: patched `libifdvpcd.so` deployed by runbook (#359) |
| Multi-reader naming | One conf line per port; name = `FRIENDLYNAME` + pcscd slot index (`GemPCTwin serial 00 00`); must use renumber-proof by-id `DEVICENAME` (AGENTS.md) | One node per USB device, auto; identity from USB strings/descriptor | Slots = ports (35963/35964 → `00 00`/`00 01`); **slot lottery** after every pcscd restart — script real-connect detection (runbook) |
| Error recovery | Strict parser, no resync → any stray byte loses the session; retry storms handled by our retry-idempotent endpoint (#101); recovery = pcscd restart | Per-transfer errors, Abort pipe, hotplug re-add; time-extension keeps slow cards alive | Eject-on-error + reconnect by design; in practice needed two upstream fixes (#359) + `gpgconf --kill scdaemon` dances (runbook) |
| PIN-pad | **Impossible**: `bPINSupport=0x00` is synthesized host-side — the host believes its own fiction (`docs/PCSC_HOST_CONTRACT_AUDIT.md`) **[ours]** | Descriptor-declared; Cherry ST-2xxx profile = `0x03` (verify+modify) — `docs/PINPAD-ARCHITECTURE.md` | No PIN-pad concept in the protocol (virtual reader, no keypad) **[ours]** |
| Max practical throughput (bench) | m5stick: 12/12 SELECTs @36.8 ms, 100 b2b @42.4 ms zero errors; card-bound floor 18–25 ms (#120); soak 1000/1000 post-#101 | ACR1252 proxy ~2 ms/APDU; our gap is firmware (~10 ms), not transport (#51) | 7,914-read overnight soak OK; ~2 s/APDU until early-return lands (runbook) |
| Silicon constraints | C3: DTR/RTS open resets the chip (`rst:0x15`), no disable bit → **broker mandatory** (#117); FTDI FT232 DTR wedge after flash = physical replug (#12) | Needs a real USB device controller (STM32 yes; C3 no; S2/S3/C6 yes) | None on the wire; board must keep TCP alive (no FIN on reset → half-open) |
| Extra host processes | 0 (or 1 broker daemon on C3: `nucula-ccid-broker`) | 0 | 0 (after #359; before: the socat/watchdog soup it replaced, `docs/transport-research-154.md`) |
| T=1 ownership | Host engine + device-side S-block endpoint (weaker spot, #101/#345) | Descriptor-chosen (we ship TPDU level) | None — full-APDU transparency |
| Reset semantics anchor | pcscd keeps card powered between connections; device must reset T=1 state at `SetParameters` re-arm (#330/#345) | `IccPowerOff/On` per connect, abort pipe | `CTRL_ON/OFF/RESET`; eject = teardown+re-accept |

---

## 3. Limitations we hit in practice (each with its issue)

1. **B115200 hardcode** — #54. `ccid_serial.c:860-863` pins every serial
   reader to B115200; reader.conf has no speed knob. The verbatim
   reference (upstream `1a0f75d`, `OpenSerialByName`, after the
   GemCoreSIMPro2 arm):

   ```c
   /* set serial port speed to 115200 bauds */
   (void)cfsetspeed(&current_termios, B115200);
   ```

   `cfsetspeed` here is UNCONDITIONAL — it runs for every serial reader
   type, after and regardless of any negotiation, so no reader.conf
   trick or readerID declaration can raise a stock pcscd host above
   115200. The measured gain ceiling is ~4.6 ms/APDU (55 wire bytes,
   5.25 → 0.66 ms at 921600); a local libccid fork was costed and
   **rejected** — permanent host-fork maintenance any package update
   silently reverts. The ceiling is HONEST AND TOTAL until upstream
   gains a speed knob (filed as an owner-gated upstream ask, §4.3).
   **Firmware side is implemented end-to-end** (2026-10-28, `wt/serial-transport`):
   the GemCore-SIMPro2-shaped escape negotiation `[0x05, code]`
   (`ccid_handler.rs` `ESCAPE_SET_BAUD`/`BAUD_TABLE`, codes 0x00–0x08 →
   115200–3 Mbaud): the reader ACKs at the CURRENT baud
   (`[0x05, code, baud u32-LE]`), then the UART serve loop switches
   only after the ack clocked out (`ack-before-switch`, the 0xD1
   flush discipline). Inert against stock pcscd by construction — the
   host never sends it — so production wire behavior is unchanged;
   direct-serial tooling exercises it today, and a patched libccidtwin
   gets its reader half ready-made.
2. **No-resync strict parser / clean wire** — #131 (decision), AGENTS.md
   "Clean-wire doctrine". Boot-log ASCII between frames desyncs pcscd for a
   whole session (`ReadSerial() Got 0x76/0x65/0x64…` = boot text
   mid-session). Resolution-by-construction: log ceiling before the driver
   claim, `test_wire_silence_after_banner` locks it; log_shim retired.
3. **Port-open chip reset (C3 reset wall)** — #117. Every pcscd port-open
   of the CDC resets the chip (`rst:0x15 USB_UART_CHIP_RESET`); the C3
   register file has no `CHIP_RST_DIS` (C6/C61 only). Direct
   libccidtwin-on-CDC is impossible; the PTY broker
   (`host-tools/nucula-ccid-broker.py`) is the permanent wired path for
   this board. Full analysis: `docs/NOCULA_USB.md`.
4. **DTR hazards** — #12 (espflash DTR/RTS toggles wedge the FTDI FT232;
   only physical replug recovers) and its daily-life cousin: pcscd holds
   the port with DTR asserted, so flashing needs the reader.conf disabled
   or pcscd stopped (AGENTS.md bench rows). The broker is DTR-blind by
   construction.
5. **FTDI latency_timer** — #51 + AGENTS.md: ftdi_sio's `latency_timer`
   defaults to 4 ms; must be 1 for the serial lane; **any USB
   re-enumeration (rebind, replug) resets it to 4** and pcscd error-loops
   until it is set back. Tracked as a bench restore step
   (`flash_and_test.sh`).
6. **vpcd EH-thread wedges** — #359 (two upstream libifdvpcd bugs:
   empty-ATR answer → `vicc_eject()` teardown on every early presence
   probe; unbounded `recvall` + `listen(…,0)` wedges pcscd's EH thread
   forever on one unanswered probe) and #326 (ATR-dead regression;
   post-disconnect re-detect decay still open). Plus bench-only stock
   driver findings: client-mode `FD_SETSIZE` abort, half-open TCP after
   board reset → `SCardEstablishContext` hangs until pcscd restart
   (runbook). Patches bench-deployed; upstream report queued (member-org
   rule: the owner files it).
7. **Reconnect semantics gap (fixed)** — #330 → PR #345: long-lived pcscd
   keeps the card powered between connections, so device-side T=1 state
   survived reconnects while libccidtwin reset its counters per
   SCardConnect. Stale counters → R-block churn → `6A88`/`6E00`. Fix:
   reset the T=1 endpoint on `SetParameters(T=1)` — same anchor as the
   host. Regression test fails without the fix (46/46 with).
8. **CDC serve-loop floor (broker lane)** — #119: byte-at-a-time
   `usb.read()` puts a ~300 ms/command floor on the nucula CDC path
   (13-byte frame ≈ 13 × ~23 ms). Not a GemPC-framing cost — a serve-loop
   cost; fix is buffered reads.
9. **Boot-window race** — #101 (also #78): pcscd starting during the
   board's boot banner fails reader init (`Get firmware failed`); one
   restart recovers. Systemd restart-on-failure is the bench mitigation.

---

## 4. Opportunities scan — what others do, what to adopt

### 4.1 osmo-ccid-firmware (in-tree `reference/osmo-ccid-firmware` @ `639db14`)

- **What they are**: portable C USB-CCID *device* implementation
  (`ccid_common/`), shipped as an 8-slot reader firmware (sysmoOCTSIM,
  SAMD51) **and** as a Linux userspace USB gadget via FunctionFS
  (`ccid_host/`) — [gitea.osmocom.org/sim-card/osmo-ccid-firmware](https://gitea.osmocom.org/sim-card/osmo-ccid-firmware).
- **Serial?** None. They do **not** emulate a GemPC Twin — their host-side
  assumption is *stock libccid over a real USB CCID interface*, plus
  vendor extensions carried in CCID `Escape` (their `ccid_common/ccid_proto.h`
  is our `CCID_SPEC` specquote source).
- **What to adopt**:
  - `sysmoOCTSIM/check_libccid_config.py` — a **driver-conformance
    test**: cross-check the device's declared config against what libccid
    will accept. We should grow the equivalent: our conformance battery
    (`tests/hardware/nfc/conformance_battery.py`) already plays pcscd; a
    descriptor-vs-libccid-expectations check is the missing half.
  - Their ISO 7816-3 Fi/Di/WT tables and slot FSM (`iso7816_3.*`,
    `ccid_slot_fsm.c`) are already specquote sources — keep mining them
    for ICC_MUTE timeout shapes.
  - The FunctionFS gadget lane proves a **host-OS gadget** (any Linux SBC)
    can serve CCID with our firmware behind it — a bench tool, not a
    product path (process count, see `docs/transport-research-154.md`).

### 4.2 vsmartcard — vpcd / vpcd-config ([repo](https://github.com/frankmorgner/vsmartcard))

- Already our wifi lane's protocol and driver (§1.3). What remains to
  adopt: **upstream the #359 fixes** (empty-ATR eject, bounded recv,
  `listen` backlog) — both are small, general fixes to frankmorgner's
  driver; and adopt `vpcd-config`'s UX idea (generate reader.conf from
  the local IP) for our runbook generator.
- Their **reversed connection** mode (driver listens, card side connects
  out) is what makes NAT-free board→host serving work — we use it; the
  Android remote-reader app is the upstream proof it scales to flaky
  links ([vsmartcard README](https://github.com/frankmorgner/vsmartcard/blob/master/virtualsmartcard/doc/README.txt.in)).

### 4.3 pcsc-lite / libccid upstream ([CCID](https://github.com/LudovicRousseau/CCID))

- **Serial is not dead upstream — it just isn't Twin**: since 1.4.27
  (2017) the *main* libccid driver supports the **Microchip SEC1210
  CCID bridge over UART** ("Add Microchip SEC1210 UART support (when
  connected on a serial port)",
  [release notes](https://blog.apdu.fr/posts/2017/05/new-version-of-libccid-1427/));
  1.7.0 (2025) added the single-slot SEC1210URT variant
  ([release notes](https://blog.apdu.fr/posts/2025/10/new-version-of-libccid-170/)).
  **[ours]** Read: upstream will take serial-attached *CCID* seriously, and
  a Baud-knob/escape-negotiation patch for `ccid_serial.c` in the
  SIMPro2 pattern (#54) is far more upstreamable than we assumed — it
  should be filed by the owner per the member-org rule.
- The twin serial protocol was **reimplemented as a finite state machine**
  upstream years ago ("complete reimplementation of the Twin serial
  protocol using a finite state automata (code much simpler)" —
  [README history](https://github.com/LudovicRousseau/CCID/blob/master/README.md));
  our `ccid-transport-serial` parser states mirror it — worth a diff
  against the current FSM when we next touch framing.
- **Descriptor quirk knobs exist as precedent**: `./configure --enable-zlp`
  for CT30/K30 Zero-Length-Packet behavior — the same shape a
  `--enable-baud-negotiation` would take.

### 4.4 Minimal / alternative CCID host stacks

- **Google Smart Card Connector** (Chromium/ChromeOS): a full port of
  pcsc-lite + libccid + libusb into a userspace extension — no pcscd, no
  root, Apache-2.0 ([repo](https://github.com/GoogleChromeLabs/chromeos_smart_card_connector),
  [Rousseau's walkthrough](https://blog.apdu.fr/posts/2017/03/pcsc-sample-in-smart-card-connector-on/)).
  Google's supported-reader list is literally generated from the CCID
  driver's files ([support page](https://support.google.com/chrome/a/answer/7014689)).
  **[ours]** Two lessons: (a) the whole host stack we conform to can run
  anywhere — a bench-side libccid under our control is a legitimate test
  harness; (b) the ecosystem's compatibility target *is* libccid —
  conforming to it (as our STM32 identities do) buys every platform at
  once.
- **Rust embedded**: [`usbd-ccid`](https://lib.rs/crates/usbd-ccid) (USB
  CCID class on `usb-device`), and
  [EspUsbDevice](https://registry.platformio.org/libraries/tanakamasayuki/EspUsbDevice),
  which adds a **CCID class via TinyUSB's application class-driver hook —
  no TinyUSB patching — on ESP32-S2/S3/P4** (one slot, T=1, short APDU,
  271-B messages). This is the concrete existence proof for the #117
  "native USB-CCID on OTG silicon" path with the least new code.

### 4.5 YubiKey / Nitrokey / SoloKeys — USB-CCID + vendor escapes

- **YubiKey**: one composite USB device — HID keyboard + FIDO HID + **CCID
  smart-card interface**, all apps (PIV/OATH/OpenPGP/Management) reached
  as APDUs *through CCID*; PIV "identifies as a Microsoft USB CCID smart
  card reader … using the base Microsoft driver" — driverless on Windows
  ([YESDK physical interfaces](https://docs.yubico.com/yesdk/users-manual/yubikey-reference/transports/overview.html),
  [yubico.com PIV](https://www.yubico.com/authentication-standards/smart-card/)).
  **What to adopt**: their management plane rides *standard CCID APDUs to
  an AID* — the clean vendor-channel pattern. Our Escape 0x02/0xD0/0xD1/D2/D3
  vendor plane is the same idea one level down; on the USB lane we could
  move identity/counters to a management AID and keep `Escape` for
  spec-shaped needs.
- **Nitrokey 3 / SoloKeys**: Rust firmware (Trussed framework, LPC55,
  developed with SoloKeys — [nitrokey-3-firmware](https://github.com/Nitrokey/nitrokey-3-firmware)).
  Notably, firmware **1.9.1 "fixes USB CCID descriptor compatibility on
  Windows"** ([release](https://github.com/Nitrokey/nitrokey-3-firmware/releases/tag/v1.9.1))
  — i.e. a shipping vendor got bitten by descriptor conformance late.
  **What to adopt**: descriptor conformance regression tests on *all* our
  claimed identities (Cherry/CT30/K30), not just Linux+pcscd; the
  descriptor is the whole contract on the USB lane (§1.1).
- **Solo2** ([solokeys/solo2](https://github.com/solokeys/solo2)) — the
  Rust/LPC55 reference for CCID+vendor classes on a security key form
  factor.

### 4.6 ELATEC / Identiv — what commercial readers do with serial

- **ELATEC TWN4** family: USB operating modes are "USB keyboard emulation –
  USB virtual COM port – **CCID / PC/SC 2.01**"; the RS-232 variant runs
  at up to 115,200 baud and speaks ELATEC's **own SDK/ASCII protocol**,
  not PC/SC ([TWN4 MultiTech datasheet](https://www.elatec-rfid.com/fileadmin/Documents/Data-Sheet/elatec-twn4-multitech-data-sheet-en.pdf),
  [SmartCard family datasheet](https://www.elatec-rfid.com/fileadmin/Documents/Data-Sheet/elatec-twn4-multitech-smartcard-data-sheet-en.pdf)).
- **Identiv**: readers ship as USB CCID class devices; even their newest
  "Identiv SmartOS Reader" entered libccid's supported list in 1.7.0
  ([release notes](https://blog.apdu.fr/posts/2025/10/new-version-of-libccid-170/)).
- **[ours] The lesson**: the industry converged decades ago — PC/SC rides
  USB CCID; serial links get *vendor* protocols. Twin emulation is a
  bench-era choice that bought us pcscd compatibility on silicon with no
  USB controller; it is not a pattern anyone else carries forward.

---

## 5. Recommendation (ranked)

1. **Keep & harden the USB-CCID class lane (STM32) — it is the reference
   architecture.** Zero-config, hotplug, PIN-pad-capable, descriptor-driven.
   Invest in descriptor conformance across all three claimed identities
   (Nitrokey's Windows lesson, §4.5) and steal osmo's
   `check_libccid_config.py` idea for a libccid-expectations test (§4.1).
2. **Plan the twin-lane replacement for the next board rev: native USB-CCID
   on OTG silicon (S2/S3) or C6-with-`CHIP_RST_DIS`.** The EspUsbDevice
   TinyUSB app-class hook (§4.4) is the shortest path; `usbd-ccid` the Rust
   purist's path. This deletes the broker, the FTDI quirks, and the C3
   reset wall in one move (#117, `docs/NOCULA_USB.md` §3).
3. **Keep & harden VPCD as the network lane; upstream the #359 driver
   fixes** (owner files upstream per member-org rule) and close the #326
   re-detect decay + early-return (~2 s → ms-class APDU). VPCD is the only
   lane with no T=1-on-the-wire and full APDU transparency — right for wifi
   and for any future remote-reader product.
4. **Keep the m5stick twin lane as a supported legacy lane, but treat #54
   as an upstream play, not a local fork.** The SIMPro2 escape-negotiation
   pattern is in-tree evidence the patch is upstreamable (§4.3); our own
   cost-benefit already rejected a local fork once. Maintain clean-wire +
   `latency_timer=1` discipline (§3) as long as the lane lives.
5. **Freeze the PTY broker at maintenance** — it is the correct permanent
   answer *for C3 only*; new boards should exit via (2), not extend it.

---

## Sources

**In-tree (pins in `reference/README.md`)**: `reference/CCID/` @ `1a0f75d`
(ccid_serial.c:620-642, 795-870; ifdhandler.c:1649-1728; openct/proto-t1.c;
readers/*.txt; README history) · `reference/pcsc-lite/` @ `6cadd81`
(readerfactory.c:1484-1525; pcscd.h.in:53; eventhandler.c) ·
`reference/osmo-ccid-firmware/` @ `639db14` · `AGENTS.md` (transport
landscape, clean-wire doctrine, C3 wall, role doctrine) ·
`docs/PCSC_HOST_CONTRACT_AUDIT.md` · `docs/VPCD_ALIGNMENT_AUDIT.md` ·
`docs/VPCD_WIFI_RUNBOOK.md` · `docs/transport-research-154.md` ·
`docs/NOCULA_USB.md` · `docs/T1_BEST_PRACTICES.md`.

**Issues/PRs**: #12 (FTDI wedge) · #51 (perf decomposition) · #54 (B115200) ·
#78/#101 (boot race, T=1 wall + RX hardening) · #117 (C3 reset wall, broker) ·
#119 (CDC floor) · #120 (card-bound floor) · #131 (clean-wire decision) ·
#326 (wifi-vpcd ATR regression) · #330/#345 (reconnect T=1 reset fix) ·
#359 (libifdvpcd patches).

**External**: [reader.conf(5)](https://manpages.opensuse.org/Tumbleweed/pcsc-lite/reader.conf.5.en.html) ·
[USB CCID 1.1 spec](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf) ·
[libccid](https://github.com/LudovicRousseau/CCID) ([supported readers](https://ccid.apdu.fr/ccid/supported.html)) ·
[libccid 1.4.27](https://blog.apdu.fr/posts/2017/05/new-version-of-libccid-1427/) ·
[libccid 1.7.0](https://blog.apdu.fr/posts/2025/10/new-version-of-libccid-170/) ·
[vsmartcard](https://github.com/frankmorgner/vsmartcard)
([README](https://github.com/frankmorgner/vsmartcard/blob/master/virtualsmartcard/doc/README.txt.in),
[vpcd-config.c](https://github.com/frankmorgner/vsmartcard/blob/7c949d1e/virtualsmartcard/src/vpcd-config/vpcd-config.c)) ·
[osmo-ccid-firmware](https://gitea.osmocom.org/sim-card/osmo-ccid-firmware) ·
[Smart Card Connector](https://github.com/GoogleChromeLabs/chromeos_smart_card_connector)
([walkthrough](https://blog.apdu.fr/posts/2017/03/pcsc-sample-in-smart-card-connector-on/),
[ChromeOS reader list](https://support.google.com/chrome/a/answer/7014689)) ·
[Yubico transports](https://docs.yubico.com/yesdk/users-manual/yubikey-reference/transports/overview.html) ·
[Yubico PIV](https://www.yubico.com/authentication-standards/smart-card/) ·
[Nitrokey 3 firmware](https://github.com/Nitrokey/nitrokey-3-firmware)
([v1.9.1](https://github.com/Nitrokey/nitrokey-3-firmware/releases/tag/v1.9.1)) ·
[solo2](https://github.com/solokeys/solo2) ·
[ELATEC TWN4 datasheets](https://www.elatec-rfid.com/fileadmin/Documents/Data-Sheet/elatec-twn4-multitech-data-sheet-en.pdf) ·
[usbd-ccid](https://lib.rs/crates/usbd-ccid) ·
[EspUsbDevice](https://registry.platformio.org/libraries/tanakamasayuki/EspUsbDevice).
