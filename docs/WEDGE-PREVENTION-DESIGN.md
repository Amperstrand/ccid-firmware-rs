# Wedge Prevention Design — host-side reader uptime (monitor v4, flash taxonomy, console tap)

| | |
|---|---|
| **Status** | **DRAFT — design only.** Nothing in this document is implemented or bench-verified. Every verification item is a TODO (§7). |
| **Kanban** | `t_6cafe4eb` — "wedge prevention (software, no purchases) — monitor v4 persistent-open + esp32_flash.py failure diagnosis + ser2net console daemon" |
| **Constraint** | **Software-only. No purchases**: no hardware taps, no additional USB-UART bridges, no inline analyzers, no new silicon. Everything is host-side code (daemons, systemd units, udev rules, docs) on the bench host against devices already on it. |
| **Doctrine hooks** | Clean-wire / log-ceiling (AGENTS.md "Clean-wire doctrine", `docs/LOGGING_DOCTRINE.md`); one hardware holder (labgrid places); pcscd flock serialization; reference-first (cite before inventing). |

---

## 0. The wedge classes and the prior art

### 0.1 Failure model — why readers "wedge" from the host side

The bench's serial CCID readers hang off USB-UART bridges (m5stick → Hades2001
FTDI `usb-Hades2001_M5stack_49D6163EBE-if00-port0`; nucula → Espressif
USB-JTAG/CDC `usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:72:9A:50:18-if00`).
Three host-side mechanisms repeatedly take them down:

1. **Re-enumeration churn resets driver tunables.** Any USB re-enumeration
   (ftdi_sio unbind/rebind, replug, adapter reset, flash-with-reset) RESETS the
   FTDI `latency_timer` sysfs node from the required 1 back to the driver
   default 4 — and pcscd error-loops until it is set back (issue #51, the
   latency_timer class; AGENTS.md "The clean-wire rule (UART mains + pcscd)":
   *"Only the latency_timer still needs manual re-set after a rebind"*). The
   same mechanism class applies to any usb-serial driver whose per-port
   tunables revert on re-bind — the kanban names CP210x alongside FTDI for
   exactly this reason: `cp210x` re-derives its port settings on every
   re-attach the way `ftdi_sio` re-defaults `latency_timer`. (The ccid bench
   itself currently carries only FTDI and Espressif-CDC bridges; CP210x is the
   generalization, not a bench device.)
2. **Open/close transitions are themselves a wedge vector.** Every open() and
   close() of the FT232 fires DTR/RTS TIOCMSET transitions that corrupt the
   bridge's USB state machine — 11 bus disconnects in one lab kernel log,
   timestamped to debugging sessions; only a USB rebind recovers (bolty-rs
   `docs/lessons-learned.md` **B11**). On the ESP32 side, a DTR-asserted open
   plus an RTS pulse parks the chip in download mode (`boot:0x5`), and on the
   C3 a DTR-asserted holder latches ROM download mode across resets — the
   2026-10-09 "download-latch via DTR-asserted port holder" events (AGENTS.md
   Recovery events; bolty-rs **B13**).
3. **Re-enumeration races pcscd init.** Re-enumeration renumbers `ttyUSBx`
   (solved since 2026-10-09 by by-id paths everywhere), but the disappearance
   window still (a) kills pcscd's open fd mid-session, (b) lets udev hotplug
   re-offer the port to pcscd while the bridge is still re-initializing, and
   (c) can leave pcscd's `DEVICENAME` pointing at a device node that vanishes —
   pcscd exits fatally on a missing DEVICENAME, systemd start-limit-hits, and
   every client sees "Access denied" (bolty-rs **B13** rule 3).

Additionally, on the C3 CDC: a wedged USB-CDC never drains its TX buffer, and a
`write_timeout=None` write then blocks forever — the 2026-10-20 c3 incident
(`tests/hardware/labgrid/gempc_smoke.py` `open_safe`).

**The unifying insight** (already proven twice on this bench): the churn lives
in the open/close/re-enumerate cycle, so the counter is **one persistent,
correctly-opened owner per device** — never closed, never line-toggled — with
everything else (pcscd included) talking to a stable surrogate (PTY) or a
read-only mirror. The nucula has run exactly this shape since 2026-10-11
(`host-tools/nucula-ccid-broker.py`, issue #117); bolty-rs has run it for the
FT232 since 2026-08-25 (`bolty-console`, B11).

### 0.2 Prior art searched and found

Searched `find /root/src /tmp/opencode -iname '*monitor*' -o -iname '*wedge*'
-o -iname '*ser2net*'` (2026-10-21) plus repo greps. The load-bearing items:

| Prior art | What it proves | Where |
|---|---|---|
| **nucula-ccid-broker** (this repo) | Single persistent owner + PTY surrogate defeats the open/close churn class; safe opener (DTR/RTS deasserted before open); serving-gated symlink; `~0.5 ms` relay overhead; `>5.5 min` stability bench-proven (#117). It is the **direct template for monitor v4**. | `host-tools/nucula-ccid-broker.py`, `nucula-ccid-broker.service`, AGENTS.md Nucula C3 hardware wall |
| **bolty-console** (bolty-rs) | FT232 single-open console daemon over a unix socket; RX journal ring; `PING` health command; `RAW <sec>` passive capture; ModemManager `ID_MM_DEVICE_IGNORE` udev rule; DTR/RTS never touched after the one open. It is the **direct template for the console tap (component 3)** and half of monitor v4. | `bolty-rs/tools/hil/bolty-console.py`, `docs/lessons-learned.md` B11 |
| **bolty overnight watchdog** | Health-surface shape: heartbeat freshness, dead-man pid, bounded recovery budget ("hard cap 2 recoveries/night, then passive-monitor degrade"), CCID-window monitor polling `readers()`. Template for monitor v4's health surface semantics. | `bolty-rs/tools/hil/overnight/watchdog.py` |
| **microfips overnight_monitor** | The monitor lineage (v1-class): plain serial capture + periodic stats sampling to TSV. What v4 evolves beyond: passive logger → persistent owner with health + guards. | `/tmp/opencode/microfips/scripts/overnight_monitor.py` |
| **esp32_flash.py** (this repo) | The complete existing flash-side wedge discipline: quiesce/restore, baud cap, write-first/erase-last ordering, the USB latch ladder (retry → USBDEVFS_RESET → ftdi rebind → retry → replug verdict), latency_timer restore, boot checks, streaming log, stale-marker recovery. **Component 2 documents this; it does not add code.** | `tests/hardware/labgrid/esp32_flash.py`, AGENTS.md "ESP32 serial wedge table" |
| **stm32_flash.py** (this repo) | The F469 counterpart wedge table (SYSRESETREQ USB wedge, SWD lockout, ST-LINK latch via USBDEVFS_RESET, xHCI port wedge) — cross-referenced by the taxonomy. | `tests/hardware/labgrid/stm32_flash.py`, AGENTS.md F469 wedge table |
| **USB PHY reset pattern** (this repo) | The firmware-side self-healing counterpart: in-firmware OTG FS PHY reset on every boot so a soft-reset flash still re-enumerates (#22). Monitor v4 is the host-side complement — the two meet at the re-enumeration boundary. | AGENTS.md "USB PHY Reset Pattern (issue #22)", `firmware/ccid-firmware/src/main.rs` |
| **gempc_smoke safe opener** | Bounded-loud I/O on possibly-wedged CDC: `write_timeout=2.0`, DTR/RTS deasserted before open — every probe fails in bounded time instead of hanging. | `tests/hardware/labgrid/gempc_smoke.py` `open_safe` |
| **Clean-wire doctrine** | The console tap's hard constraint: the CCID wire carries CCID frames ONLY; libccidtwin's parser is strict with NO resync; diagnostics are PULL or post-mortem, never push. | AGENTS.md "Clean-wire doctrine", `docs/LOGGING_DOCTRINE.md` |

No in-repo `ser2net` prior art exists (no hits); no monitor v1–v3 artifacts
exist in this repo — the "v4" in the kanban title counts across the fleet
lineage above (microfips logger → bolty watchdog → nucula broker → this
design).

---

## 1. Component 1 — monitor v4: persistent-open reader keeper

### 1.1 Goal

A single host daemon per serial CCID device that:

1. **holds the device open forever** (one open per boot, B11 rule 1) with the
   safe opener discipline (DTR/RTS deasserted BEFORE open — never touched
   after),
2. **defeats the re-enumeration churn**: detects the device-vanished event,
   re-opens via the by-id path, and **restores `latency_timer=1` immediately
   on the new node** (before pcscd or any consumer sees it), and
3. exposes a **health surface**: last-seen frame timestamps, uptime counters,
   re-enumeration/recovery ledger.

### 1.2 Why persistent-open is the right shape (grounding)

- **#51 latency_timer class**: after a rebind, pcscd error-loops until
  `latency_timer` is 1 again. Today that restoration is a manual step
  (`esp32_flash.set_ftdi_latency` does it after flashes; an arbitrary
  rebind/replug does not). A resident daemon watching the sysfs node closes
  the gap for EVERY re-enumeration, not just the flash path.
- **m5stick re-enumeration notes** (AGENTS.md): re-enumeration renumbers
  `ttyUSBx` (mitigated by by-id paths) and resets `latency_timer` (NOT
  mitigated outside the flash path). The m5stick is also the board that gets
  re-flashed/re-purposed by sibling projects (micronuts re-flashed it within
  ~30 min of a release, 2026-10-09) — churn is routine, not exceptional.
- **B11**: open/close TIOCMSET transitions themselves corrupt the FT232 USB
  state machine. Every bench script that opens the raw tty to "check" the
  reader is a wedge source. One owner, opened once, removes the entire class.
- **B13 rule 3 / pcscd fatality**: pcscd dies fatally when its `DEVICENAME`
  vanishes. Serving pcscd from a PTY whose path never changes removes the
  failure mode (proven: the nucula broker's `/dev/nucula-ccid` symlink +
  `reader.conf.d/nucula-broker` entry).

### 1.3 Architecture

```
                    ┌──────────────────────────── monitor v4 ────────────────────────────┐
                    │                                                                    │
 /dev/serial/by-id/ │  safe open (dtr=False, rts=False BEFORE open)                     │
   usb-Hades2001… ──┼─► [relay thread] ◄──────────────► /dev/m5stick-ccid  (PTY +        │
   (the CCID wire)  │   byte-transparent both ways        stable symlink — pcscd's        │
                    │   passively timestamps frames        DEVICENAME, from reader.conf   │
                    │                                                                    │
                    │  [RX journal] ──► ring + on-disk journal ──► component 3 tap        │
                    │  [health]      ──► last-seen frames, counters, enum ledger          │
                    │  [guard]       ──► latency_timer watcher, re-open on vanish         │
                    └────────────────────────────────────────────────────────────────────┘
                                            ▲
                       pcscd (libccidtwin) ──┘ talks ONLY to the PTY (line control = no-op)
```

- **Owner**: monitor v4 is the ONLY process that ever opens the raw device
  node (the nucula broker's "safe opener" role, generalized to FTDI).
  `esp32_flash.quiesce_unit` (#314) already models exactly this hand-off:
  flash sessions stop the owning unit under the bench flock, flash, restart
  it. Monitor v4 registers itself as a `quiesce_unit`.
- **Surrogate**: pcscd's `reader.conf.d` entry points at the daemon's stable
  symlink (PTY for FTDI, exactly the broker's `/dev/nucula-ccid` pattern).
  On the PTY, libccidtwin's termios/TIOCMSET line control is a harmless
  no-op — this is what defuses the DTR/RTS reset idiom on the real wire
  (bench-proven on the nucula: `rst:0x15` vortex → PTY broker → stable).
- **Re-enumeration handling**: the relay thread's read fails when the device
  vanishes (B11's observed `SerialException`) → the daemon does NOT exit;
  it polls the by-id path (the `ConsoleCapture` 50 ms pattern from
  `tests/hardware/nucula/console.py`), re-opens safe, restores
  `latency_timer=1` on the new sysfs node (reuse `set_ftdi_latency`
  semantics), increments the ledger, and keeps serving the SAME PTY — pcscd's
  session survives the churn instead of racing it. This is the "defeats the
  churn AND the pcscd-init race" property: the race window moves inside the
  daemon where it is supervised, instead of between udev and pcscd where
  nobody is.
- **Fleet placement**: one systemd unit per device
  (`ccid-monitor@m5stick.service` template unit), `Restart=always`,
  `After=` the device unit; udev rule adds `ID_MM_DEVICE_IGNORE` for the
  bench bridges (B11 rule 2 — ModemManager probes each new ttyUSB
  enumeration and races real clients).

### 1.4 Health surface (schema, draft)

JSON status endpoint (unix socket `PING`-style + periodic dump to
`/var/lib/ccid-monitor/<device>.json`), fields:

```jsonc
{
  "device": "usb-Hades2001_M5stack_49D6163EBE-if00-port0",
  "opened_at": 1760000000.0,          // single-open epoch (B11 rule 1)
  "uptime_s": 3600,
  "last_frame_rx": 1760003599.5,      // last LRC-valid RDR frame seen ON THE WIRE
  "last_frame_tx": 1760003599.4,      // last host frame relayed
  "frames": {                          // uptime counters (monotonic since open)
    "rx_total": 1204, "tx_total": 1200,
    "rx_lrc_bad": 0,                  // corruption signal (clean-wire health)
    "rx_by_type": {"0x81": 600, "0x82": 604}
  },
  "pcscd_sessions": 3,                 // PTY peer open/close count
  "reenum_events": [                   // the churn ledger
    {"t": 1760001200.0, "cause": "read-failed",
     "reopen_ms": 850, "latency_timer_fixed": true}
  ],
  "latency_timer": {"value": 1, "last_checked": 1760003550.0},
  "recoveries": {"total": 0}           // bounded like the bolty watchdog budget
}
```

- Frame timestamps use the existing structurally-valid-frame scanner (the
  broker's `first_rdr_frame` LRC/SYNC-anchored parse — never assume a clean
  stream, AGENTS.md "Reader test timing").
- **Passive by default.** The health surface derives from observed relay
  traffic. An ACTIVE probe mode (framed `GetSlotStatus`, or Escape 0xD0
  counters — protocol-legal, in-band, PULL per the doctrine) is design-gated:
  only while the PTY has NO peer (no pcscd session), never interleaved with a
  live session. Default off until bench-verified (§7).
- Escape 0xD0 counter relays (apdu_tx/rx, nak, reinit, presence, uptime — the
  28-byte diagnostics struct) are the firmware-side complement and the
  intended cross-check for the daemon's own counters.

### 1.5 What monitor v4 must NOT do

- Never touch DTR/RTS after the single open (B11 rule 3; the C3 latch class).
- Never write anything to the wire that is not a byte-transparent relay of a
  PTY peer frame (clean-wire: no banners, no probes outside the gated mode).
- Never "help" pcscd by re-opening it, restarting it, or editing
  `reader.conf.d` (pcscd lifecycle belongs to the bench flock discipline).
- No unbounded recovery loops: recovery actions are budgeted (bolty-watchdog
  pattern: hard cap, then passive-monitor degrade with the event recorded).

### 1.6 The m5stick PTY question (open design decision)

Serving the m5stick through a PTY adds one relay hop on the GemPC wire. The
nucula broker measured `~0.5 ms` p50 overhead on CDC; the m5stick's own
per-APDU floor is ~36.8–42 ms (bench evidence, AGENTS.md m5stick row), so a
sub-millisecond relay is noise there — but this is a CLAIM TO VERIFY (§7),
not a fact, and the fallback (monitor as pure observer + latency_timer guard,
pcscd keeps the raw device) is the phase-1 shape if the PTY regresses
anything. The decision rides on one bench measurement, not on preference.

---

## 2. Component 2 — esp32_flash failure diagnosis taxonomy

### 2.1 What exists (and is cited, not re-invented)

`tests/hardware/labgrid/esp32_flash.py` already ENCODES the recovery
machinery (AGENTS.md "ESP32 serial wedge table", 2026-10-19 m5stick session /
#278 xtensa leg):

- the **ladder** — `attempt_with_ladder()`: plain retry → `USBDEVFS_RESET`
  of the port's USB device (`usb_dev_path_by_serial` sysfs serial match,
  ioctl `0x5514`) → retry → `ftdi_sio` unbind/rebind (`ftdi_rebind`) →
  retry → loud physical-replug verdict with exact board state;
- **quiesce/restore** — `pcscd_lock_acquire/release`: BOTH pcscd units
  stopped under a bench flock held as an fd for the whole window
  (socket-activation race), `quiesce_unit` for standing port owners
  (#314), `kill_port_holders`;
- **ordering** — writes FIRST (descending offsets: app before partition
  table), destructive erases LAST (`erase_regions` after every write
  landed);
- **baud discipline** — `baud_from_exporter` caps to the exporter-pinned
  speed (115200 on the m5stick port);
- **hygiene** — `set_ftdi_latency` (issue #51) in restore;
  `recover_stale_pcscd_down` (SIGKILL'd-flash marker);
- **proof-of-boot** — `boot_check_no_conflict` (`Calling app_main()`, ROM
  banners insufficient), `wire_boot_check` (#313 console-none families);
  `StreamingLog` (2026-10-20 c3 incident: stuck-phase attribution).

What does NOT exist yet is the **diagnosis matrix as documentation**: the
ladder recovers, but the mapping *symptom → verdict → why this recovery* is
spread across the module docstring, AGENTS.md tables, and incident notes.
This component consolidates it so a failed flash session yields a
**classified verdict** someone can act on without archaeology (the module's
own stated goal: "recovery is a known quantity instead of archaeology").

### 2.2 Symptom class → instrumented verdict → recovery matrix

| # | Symptom class | Observable signature | Instrumented verdict (who produces it) | Recovery path (existing code/procedure) |
|---|---|---|---|---|
| W1 | **Port-owner race** (pcscd socket-activation respawn, standing broker, stray logger) | esptool: "device reports readiness to read but returned no data (multiple access on port?)" or open EBUSY | quiesce log lines (`$ systemctl stop … rc=`); `fuser -k` in `kill_port_holders` | `pcscd_lock_acquire` (both units, held flock) + `quiesce_unit` + `kill_port_holders` — pre-flight, always |
| W2 | **Baud flake** (bridge beyond its configured speed) | esptool: "Serial data stream stopped: possible serial noise or corruption" at 460800 | exporter-config cap log line ("baud N exceeds exporter-configured M — downgraded") | `baud_from_exporter` cap — pre-flight, always |
| W3 | **USB/FTDI latch** (persistent) | zero console bytes even across RTS pulses; `flash-id` fails; every esptool op times out | ladder log lines ("usb latch recovery: USBDEVFS_RESET … → True/False", "ftdi_sio rebind … → True/False", "retry after rebind rc=N") | ladder: retry → USBDEVFS_RESET → retry → ftdi rebind → retry; then **physical replug** (verdict text names it + board state) |
| W4 | **Dropped reset / stale firmware** | flash "succeeds" + "Hard resetting via RTS pin…" but console keeps OLD FWID/health counters | `boot_check_no_conflict` FAIL (no `Calling app_main()` evidence / old markers) or `wire_boot_check` no-reply | re-run flash; escalation per `board.flash_and_boot()` ladder (JTAG reset → `flash-id --after hard-reset`) |
| W5 | **Download-mode latch** | console: `boot:0x5, waiting for download`; reader vanished from pcscd | console capture shows the ROM prompt; DTR-holder audit (`fuser`) | `flash-id --after hard-reset` round-trip; remove DTR-asserted holders; monitor v4's never-DTR rule prevents recurrence |
| W6 | **latency_timer reset on re-enum** (post-flash) | pcscd error-loops AFTER an otherwise successful flash | `set_ftdi_latency` return in restore log | restore step (automatic); monitor v4's guard generalizes it to ALL re-enums (§1) |
| W7 | **pcscd left down** (SIGKILL'd flash) | every bench reader absent; clients "Access denied" | stale `.pcscd-down` marker next to the lock | `recover_stale_pcscd_down` on next invocation / any bench script |
| W8 | **Erase/write stranding** | board unbootable after partial flash: erased otadata + partial PT + dead serial | verdict text after write/erase failure (exact written/erased state enumerated) | prevention by ordering (writes first, erases last); recovery = full reflash ladder (bootloader+PT+app), physical replug if bridge dead |
| W9 | **Partition/otadata boot-loop** | console: `0x40000 has invalid magic byte (nothing flashed here?)` or wrong-slot boot | boot capture; partition readback from boot log (`boot: 2 factory …`) | write PT + app at the table's offsets; erase otadata selector AFTER writes (2026-10-15 restore recipe) |
| W10 | **CDC sustained-transfer wedge** (C3 class) | probe writes hang (no bounded failure); wedged CDC never drains TX | `gempc_smoke.open_safe` write timeout (2 s, loud) instead of an eternal block | bounded-loud failure → classify as W3-for-CDC → USBDEVFS_RESET / replug; never `write_timeout=None` |

Cross-reference (STM32 side, same doc shape): F469 wedge table — plain
SYSRESETREQ wedge, SWD lockout, ST-LINK dongle latch (USBDEVFS_RESET),
xHCI port wedge (PCI remove/rescan) — `stm32_flash.py`, AGENTS.md.

### 2.3 Decision tree (the flash session as a diagnosis instrument)

```
esp32 flash session failed / board misbehaves?
├── esptool could not even open the port?
│   ├── "multiple access on port?" / EBUSY ............ W1 port-owner race
│   │      → who held it: quiesce log + fuser; standing units via quiesce_unit
│   └── port node MISSING ............................. USB gone: W3 or replug
│          → ladder (USBDEVFS_RESET → ftdi rebind); still gone = physical replug
├── esptool opened, then data-phase failure?
│   ├── "Serial data stream stopped / corruption" ..... W2 baud flake
│   │      → check exporter-pinned speed; cap enforced
│   ├── every op times out, zero console bytes ........ W3 USB/FTDI latch
│   │      → ladder verdict lines; replug as last resort
│   └── hangs forever (no bounded failure) ............ W10 unbounded-write bug
│          → every probe must use bounded write_timeout (gempc_smoke rule)
├── flash reported success?
│   ├── console keeps OLD firmware markers ............ W4 dropped reset
│   ├── boot:0x5 download prompt ...................... W5 download-mode latch
│   ├── "invalid magic byte" boot-loop ................ W9 partition/otadata
│   └── boot proof OK, but pcscd error-loops now ....... W6 latency_timer
│          → restore step; monitor v4 guard prevents the non-flash cases
└── bench readers all gone after someone's session? .. W7 stale pcscd-down
       → recover_stale_pcscd_down (marker file is the verdict)
```

### 2.4 Deliverable shape

This component stays **documentation + (optionally) verdict-string
normalization**: make the ladder's existing log lines carry a stable
`W#`-class token per row above so tooling (and the nightly's amber-skip
reasoning) can count wedge classes over time. The mapping table is the
contract; the code already emits ~90% of the evidence. Verification of each
row's mapping against a live failure is TODO (§7) — several rows
(W8/W9/W10) were last proven in incident notes, not by replay.

---

## 3. Component 3 — ser2net console daemon (capture without stealing the CCID wire)

### 3.1 Requirement and the trap in the name

Requirement: network console capture for the bench boards — the ser2net
user story (TCP port per device, `telnet bench 7001` and watch) — for
post-mortems, boot evidence (FWID markers), and overnight observation.

The trap: **the m5stick wire IS the CCID wire.** UART0 through the FTDI
carries GemPC Twin protocol frames served to pcscd (libccidtwin). Stock
ser2net on that port would:

1. **steal the wire** — ser2net opens the tty as owner; pcscd's libccidtwin
   open then fails or races (the exact contention class `esp32_flash`
   quiesces away);
2. **toggle line control** — a serial open asserts DTR/RTS by default
   (pyserial does; so does ser2net's line-discipline setup): on the FT232
   that is the B11 TIOCMSET churn, on ESP32 hardware it is the
   download-mode latch request (B13 rule 1);
3. **offer an inject path** — ser2net's telnet surface is bidirectional;
   anything typed goes on the wire. On a strict parser with NO resync
   (libccidtwin), one stray byte desynchronizes pcscd for the whole session.

Doctrine grounding (clean-wire / log ceiling, AGENTS.md + 
`docs/LOGGING_DOCTRINE.md`): *a protocol wire is not a debug channel*; the
CCID wire carries CCID frames ONLY; diagnostics are PULL or POST-MORTEM.
Therefore: **a console tap on the CCID wire must NEVER inject — capture
only, by construction, not by discipline** (the same "enforced by
CONSTRUCTION" standard as the log ceiling).

### 3.2 Design: mirror, don't own

```
 CCID wire (FTDI/CDC) ── owned by monitor v4 (component 1) ──┐
                              │                                │
                        [RX journal]                    pcscd ◄─┘ (PTY relay,
                              │                                     unchanged)
                              ▼
                    ccid-console-tap (ser2net-shaped)
                    read-only TCP surface per device:
                      telnet bench 7001  →  live tail of the journal
                      GET /status        →  monitor v4 health JSON (§1.4)
                    NO path from TCP bytes to the serial wire. Hard-disabled.
```

- The tap **front-ends monitor v4's RX journal** (bolty-console's
  journal/ring + `RAW <sec>` passive-capture semantics, exposed over TCP
  instead of a unix socket). The tap opens NO serial port at all. There is
  no second opener, no line toggling, no byte competition with pcscd, and
  the inject path does not exist in the code.
- **What the tap actually shows** per board (be honest about the yield —
  clean-wire means the CCID wire is quiet post-banner by design):
  - re-enumeration windows: ROM/bootloader banners, boot banner, FWID
    markers — exactly the evidence `_boot_capture` collects today
    (`esp32_flash.py`), now visible live and journaled;
  - the daemon's own frame-level health (timestamps, LRC validity) —
    CCID-structured evidence, NOT decoded card traffic;
  - panic/coredump-path lines where the console family prints them
    (silent-reboot configs print nothing — by design; the coredump is the
    post-mortem there).
- **Never-decode rule**: the tap displays framing health (sync/LRC/length),
  not card payloads — the CCID wire can carry host-card secure-channel
  traffic (T=1 doctrine: byte-transparent relay); a console surface must
  not become a key-material observatory. Payload display = hex-dump of
  frame headers at most, counters elsewhere.
- **Where ser2net proper IS allowed**: on wires that are NOT protocol
  wires — a future dedicated console UART, or front-ending the existing
  netlog UDP :4567 sink (debug-channel matrix, AGENTS.md). Stock ser2net
  stays banned on every GemPC/CDC CCID path.
- systemd: `ccid-console-tap@<device>.service`, read-only filesystem view
  of the journal dir, no TTY device unit dependency at all.

### 3.3 Per-board applicability

| Board | CCID wire | Console surface | Tap yield |
|---|---|---|---|
| m5stick (FTDI) | UART0 via FT232 | none post-banner (clean-wire) | boot/re-enum evidence + frame health |
| nucula (C3 CDC) | USB-CDC (brokered) | boot banner only (#131 ceiling) | same, via the existing broker's stream |
| STM32 F469/F746 | USB CCID | defmt+RTT (separate from USB) | N/A — RTT is the console; ser2net out of scope |

---

## 4. How the three components compose

- **monitor v4** is the foundation: owner of the wire, keeper of the journal,
  executor of the latency_timer guard, producer of the health surface.
- **flash taxonomy (component 2)** consumes monitor v4's ledger: a W3 latch
  during a flash window and a W3-class re-enumeration outside one are the
  same event with different triggers; the matrix's verdict tokens make the
  correlation countable. Flash sessions interact with monitor v4 strictly
  through the existing `quiesce_unit` hand-off (#314 discipline) — the flash
  tool never fights the daemon for the port.
- **console tap (component 3)** is a pure consumer of monitor v4's journal —
  deployable/independent, but worthless without component 1 (except on
  non-CCID wires).

Failure isolation: each is a separate systemd unit; monitor v4 dying must
not wedge the bench worse than today's baseline (pcscd falls back to the raw
device only if the reader.conf PTY entry is swapped — a deliberate
two-state bench decision, TODO §7).

## 5. Software-only constraint (explicit)

No purchases of any kind: no hardware taps/analyzers (the tap is a journal
mirror), no additional USB-UART bridges (the PTY surrogate is software), no
new MCU silicon (C6/S3 for `USB_UART_CHIP_RST_DIS` remains a hardware path
explicitly out of scope — the broker already routes around that wall in
software, #117). Everything lands as: `host-tools/` daemons + systemd
units + udev rules + this doc + the taxonomy.

## 6. Non-goals / rejected alternatives

- **In-band log output for observability** — rejected by doctrine
  (`docs/LOGGING_DOCTRINE.md` "the alternatives, honestly compared"; the
  #91 log-shim retirement). The tap mirrors; it never asks the firmware to
  print more.
- **Active wire probes during pcscd sessions** — rejected by default (§1.4):
  protocol-legal is not session-safe; gated to peer-idle windows, off until
  verified.
- **Monitor-driven auto-reflash / auto-restart of pcscd** — rejected: the
  recovery budget pattern (bolty watchdog) bounds OBSERVATION-side
  recovery; flash and pcscd lifecycle stay under the bench flock + labgrid
  place discipline, human/CI-initiated.
- **Modifying firmware for host-side wedge relief** — out of scope here; the
  firmware side already carries its own self-healing (#22 PHY reset, 0xD0
  counters, #101 RX hardening).

## 7. Verification plan — ALL TODO (nothing below has run)

- [ ] **V1 — PTY relay cost on the m5stick GemPC wire**: brokered vs direct
      pcscd APDU round-trip (conformance battery + p50 latency vs the
      ~36.8–42 ms baseline). Gate for §1.6 (PTY vs observer-only phase 1).
- [ ] **V2 — safe-opener single-open**: monitor v4 holds the port; pcscd
      serves through the PTY; conformance battery green; zero TIOCMSET
      transitions after the initial open (verify via `strace` count).
- [ ] **V3 — re-enumeration guard**: `ftdi_sio` unbind/rebind under the
      daemon → latency_timer back to 1 within the daemon's poll interval;
      pcscd session survives (no EHStatusHandlerThread error-loop); ledger
      row written. Replug variant too.
- [ ] **V4 — health surface truth**: last-seen counters match an
      independent `Escape 0xD0` pull (idle window) and a scripted pcscd
      session's frame count.
- [ ] **V5 — taxonomy mapping**: for each W1–W10 row, replay or cite a live
      failure whose signature matches and whose verdict token appears in
      the flash log (W3/W6 replayable on demand; W8/W9/W10 need incident
      replay rigs — until then rows stay marked "incident-cited").
- [ ] **V6 — tap never-injects**: code audit (no write path from TCP to
      serial — enforced by construction) + bench proof: tap connected and
      abused (random input) while pcscd session runs → zero frame
      desync, zero wire bytes attributable to the tap.
- [ ] **V7 — tap yield**: FWID marker + boot banner visible through the TCP
      surface during a monitored re-enumeration.
- [ ] **V8 — reader.conf migration**: PTY-based entries for the m5stick
      (colons rule respected — PTY paths have none), fallback procedure to
      raw-device config documented and rehearsed.
- [ ] **V9 — Codex review pass**: this design's PR findings triaged per the
      Codex feedback protocol (reply = audit trail).

## 8. Open questions

1. Does libccidtwin's serial driver behave on a PTY the same way it did on
   the nucula's CDC-broker PTY (line-control no-op, timing tolerance at
   115200 8N2)? (V1/V2 answer this.)
2. Monitor v4 scope: m5stick first; does the nucula's EXISTING broker grow
   the health surface (preferred — one daemon per device, not two), or does
   a generic monitor wrap it? Lean: extend the broker pattern into a shared
   `host-tools/ccid-monitor.py` family.
3. Where do W# verdict tokens live — `esp32_flash.py` log lines only, or
   also surfaced in the nightly's generated STATUS.md amber-skip reasons?
4. Journal retention/rotation for the tap (disk hygiene, issue #108 class)
   — ring size on disk, prune cadence.
