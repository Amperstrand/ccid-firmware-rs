# The Debug-Channel Doctrine — logging vs. protocol wires

*Established 2026-10-12 (issues #131/#130) after the ESP-IDF 5.5.5 unification
surfaced the pattern in its purest form: the only regression the port produced
was in the logging subsystem itself.*

## TL;DR rules

1. **A protocol wire is not a debug channel.** If a strict parser reads it
   (libccidtwin/pcscd, ISO 14443, T=1), unsolicited bytes are corruption by
   definition — the parser has no resync and no tolerance.
2. **Diagnostics are pull, not push.** The host asks (in-band escape commands,
   on-demand) or recovers post-mortem (coredumps). Firmware never volunteers
   bytes onto a protocol wire.
3. **One banner, then silence.** A production build may print a boot banner
   (FWID) that ENDS before the protocol driver is claimed. After that, the
   wire carries protocol frames only — enforced by a log ceiling set
   *before* the claim, not after.
4. **Live logs live on dedicated diagnostic surfaces** (separate build or
   separate wire): `pn7160-actdiag`, `ble`, netlog, RTT/JTAG.
5. **When the debug channel becomes the bug source, remove the channel.**
   Building machinery to keep a bad channel alive (the #91 log_shim: 619
   lines + ring + drain + flush ordering + purge semantics) is complexity
   that will itself fail — on 5.5.5 it was the *only* thing that failed.

## Why logging breaks protocol wires (the mechanics)

**Strict parsers never resync.** libccidtwin's serial reader is a byte
stream with no frame recovery: any ASCII between GemPC frames desyncs pcscd
for the whole session ("Get firmware" fails permanently; the reader
error-loops). This is not a bug in the parser — strictness is correct on a
protocol wire. Bench proof: 2026-10-11 `ReadSerial() Got 0x76/0x65/0x64…`
(= boot-log text) mid-session; 2026-10-10 the #91 shim's presence-event
logs raced pcscd's handshake.

**Unsolicited output races handshakes.** A protocol session opens with a
timed handshake. Log lines emitted during that window are read as garbage
frames. This forced the "purge at ceiling" workaround and the battery
Reader's drain-until-idle reads.

**The print path perturbs timing.** Logging is not free at the moment it
matters: ESP-IDF 5.5.2 fixed a ~50 ms CPU block when printing over
USB-Serial/JTAG with no host attached — a whole timing class living inside
the print call. On this bench, console output starved during the PN7160 NCI
ladder's busy-waits, and the ladder's timing is the difference between a
chip that ACKs and a chip that NAKs forever (issue #63).

**Shared peripherals contend.** The nucula's USB-CDC is one port carrying
console + CCID. The driver claim changes print semantics mid-boot (direct
writes drop — the root of #91), the host's DTR/RTS line state is a
chip-reset hazard (#117), and every console byte is a byte the CCID serve
loop did not schedule.

**Queued lines arrive late and out-of-context.** Ring-drained logs ride out
after responses (that was log_shim's design), which is exactly "ASCII
between frames" from the parser's perspective — hence the purge-before-
serve workaround, which itself had to be fixed (2026-10-11).

## What it cost us (the ledger)

| Incident | Cost | Root class |
|---|---|---|
| pcscd desync on boot-log text (2026-10-11) | session-loss + diagnosis time | ASCII on wire |
| #91 log_shim: design, build, host tests, bench-verify | a whole subsystem | keeping logs past the claim |
| log_shim purge semantics + flush ordering fixes | two bench sessions | same |
| m5stick loses FWID probe to the ceiling | identity moved to Escape 0x02 (actually an improvement) | ceiling ordering |
| Battery/probe readers must SCAN for frames instead of reading a clean stream | permanent host-side complexity | interleaving tolerated |
| Console starvation during NCI ladder | ladder waits converted to FreeRtos delays | print-path timing |
| 5.5.5 port: log_shim drain silent — the ONLY port regression | open item → resolved by retirement (#131) | the subsystem itself |

For honesty: our real *performance* walls are NOT logging — m5stick APDU
floor is card-bound (P71 FWI=9, #120), the nucula pcscd wall is C3 silicon
(#117), CAP-upload stalls are serve-loop blocking + RX loss (#101), and the
CDC ~300 ms floor is driver-level (#119). Logging was never the speed
problem; it was the recurring *friction* problem: most ESP32-side
infrastructure bugs trace to ASCII on a protocol wire or to machinery built
to keep it there.

## The alternatives, honestly compared

| Channel | Latency | Wire impact | Cost / limits | Verdict |
|---|---|---|---|---|
| **In-band escape queries** (0xD0 counters, 0x02 identity) | on-demand, ~RTT | zero (host-initiated frames) | 28 B counter struct; must be designed in | **production default** — proven |
| **Post-mortem coredump** (0xD1 / crash → flash partition → `esp32_coredump.py retrieve`) | minutes | zero (panic after ack, silent reboot) | needs the partition + ELF; frozen instant, not a trace | **production default** — proven end-to-end, incl. on 5.5.5 |
| **Log-to-flash + retrieve** (generalization: ring in flash, esptool read) | minutes | zero | flash wear; needs structure (this is coredump with extra steps) | use coredump instead |
| **Diagnostic build** (`pn7160-actdiag`: no CDC claim, logs to console) | live | zero on the CCID build (different image) | reflash to switch; no simultaneous prod+logs | **bring-up default** — proven (the ACK-window bisection v1–v10 needed it) |
| **BLE NUS** (`ble` build, issue #66) | live | zero (CCID stays clean) | pairing + flaky bench centrals; C3 blocked (adv 0x0c, maybe svc 0.53 fix); parked | optional, experimental |
| **WiFi netlog** (UDP) | live | zero | credentials baked; radio timing/power; another failure domain | optional |
| **Dedicated UART** | live | zero on CCID wire | costs a pin + a USB-UART bridge on the board | hardware-rev option |
| **RTT / JTAG** (defmt on STM32; GDB on ESP32) | live | zero (separate debug peripheral) | debugger attached; ESP32-C3 shares its only USB port with JTAG (workable, documented) | best option WHEN a debugger is attached; STM32 default |
| **Console before driver claim** (boot banner) | boot only | bounded (ends before claim) | ROM/bootloader output is not controllable from the app | keep, as the ONLY console use |

## Decision matrix

| Phase | Channel |
|---|---|
| Production (pcscd-facing) | clean-wire ceiling + Escape 0xD0/0xD1/0x02 + coredumps |
| Bring-up / new silicon bring-up | diagnostic build (actdiag pattern) + JTAG when available |
| Bench debugging a timing bug | NOTHING live (dump-and-retrieve) — the observation must not perturb the observed |
| STM32 | defmt + RTT (separate wire — logging is genuinely free there) |

## The meta-lesson (why we built the wrong thing)

The #91 incident was "logs disappear after the driver claim." The RIGHT
answer was visible even then: the wire was cleaner without the logs, and
the diagnostics we needed (counters, dumps, on-demand identity) either
existed or were cheaper than the fix. Instead we built a 619-line shim to
preserve a channel the protocol did not want — and then spent sessions on
its purge semantics, flush ordering, ceiling races, and finally its 5.5.5
breakage. **When troubleshooting feels hard because the debug channel
interferes with the thing being debugged, the move is to delete the
channel, not to route around the interference.** Complexity added to keep
observation alive inside a time-sensitive path is complexity that will
become the next incident.

The enforcement is a regression test, not discipline:
`test_wire_silence_after_banner` (tests/hardware/nucula) asserts that
everything after the boot banner is framed CCID — no ASCII, ever. If
someone reintroduces unsolicited logging, CI/bench fails loudly.
