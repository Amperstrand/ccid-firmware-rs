# Observability — what ships, and what to use when

*Decision doc for issue #133 (2026-10-28, verified against main @ `7d5bcee6e`).
Companion to `LOGGING_DOCTRINE.md` — the doctrine doc owns the WHY and the
bans (clean wire, pull-not-push, dump-and-retrieve); this doc owns the WHAT
(the verified inventory of what actually ships), the tiered recommendation,
and the decisions on the open options. AGENTS.md's debug-channel matrix
defers to §1.1 here for the escape registry.*

## 1. The inventory (every item verified in-tree)

### 1.1 The escape registry — in-band pull channel, the production floor

Every vendor escape this firmware answers today. Dispatch lives at THREE
sites (the fragmentation #133 flags): the shared serial handler
(`crates/ccid-core/src/handler.rs::handle_escape`), the ESP32 serial wrapper
(`crates/ccid-service/src/ccid_handler.rs::process_command`), and the STM32
wrapper (`firmware/ccid-firmware/src/ccid_core.rs::handle_escape`).

| Code | Meaning | Handled at | Payload in → out | Ships on | Host consumer |
|---|---|---|---|---|---|
| `0x02` | reader firmware version | ccid-core `handler.rs` | `02` → version string | all serial/ESP32 profiles | libccidtwin open; bench identity probe (m5stick's on-wire FWID) |
| `01 01 01` | GemPC sync-notification mode | ccid-core `handler.rs` | echo + arm | serial profiles | libccidtwin |
| `1F 02` | GemPC open-hack APDU-level probe | ccid-core `handler.rs` | **deliberate FAIL** (stay TPDU; a success ack would flip libccid to Short-APDU framing) | serial profiles | libccid `ccid_open_hack_post` |
| `0xD0` | diagnostics counters | ccid-core `handler.rs` + STM32 `ccid_core.rs` (driver overlay, #344) | `D0` → 28 B `Diagnostics` LE (`ccid-core/src/diagnostics.rs`) | **both firmwares** | labgrid HIL, bench probes |
| `0xD1` | snapshot arm | ccid-core `handler.rs` | `D1` → ack, then serve-loop panic → flash coredump | ESP32 | `tests/hardware/esp32_coredump.py` (dump-and-retrieve) |
| `0xD2` | turnaround decomposition (#120) | ccid-service `ccid_handler.rs` (`turnaround.rs`) | `D2` → 32 B LE `TurnaroundStats` | ESP32 serial, MFRC522 | `tests/hardware/nfc/turnaround_probe.py` |
| `0xD3` | I2C poll-cadence probe (#120) | ccid-service `ccid_handler.rs` | `D3,N` → 3 words LE (took_us, N, fwt_ms) | ESP32, MFRC522 | `tests/hardware/labgrid/m5stick_cadence_probe.py` |
| `0xD4` | WTX advisory counters (#156) | ccid-service `ccid_handler.rs` | `D4` → 8 B LE (last, total) | ESP32, **PN7160 only** (others: CMD_NOT_SUPPORTED) | `tests/hardware/nfc/wtx_hammer.py` |
| `0x6A` | Gemalto firmware features | STM32 `ccid_core.rs` (vendor-gated 08E6) | `6A` → 21 B `GEMALTO_FIRMWARE_FEATURES` | STM32 CT30/K30 profiles | libccid `set_gemalto_firmware_features` |

Facts that fall out of the table:

1. **`0xD4` is TAKEN** (WTX counters). The #133 checklist's "trace ring +
   Escape 0xD4 dump" must allocate a different code — **`0xD5`** (see §3.1).
2. **No version byte anywhere** — every payload is parsed positionally by
   host tools; the 28 B/32 B layouts are locked by byte-exact golden tests
   (GREAT EXPECTATION pattern), which is why they have not drifted.
3. Adding an escape today means touching the dispatch `if`-chain at one of
   three sites + a bespoke probe script — no registry, no shared decoder.

### 1.2 Post-mortem (dump-and-retrieve)

- **ESP32**: coredump-to-flash, ELF + CRC32, partition `0x3F0000..0x400000`
  (`partitions-ota.csv`), `CONFIG_ESP_SYSTEM_PANIC_SILENT_REBOOT=y` in every
  `.full` config. Triggered by any crash OR by Escape 0xD1 (ack first, then
  panic). Retrieve + gdb-multiarch decode: `tests/hardware/esp32_coredump.py`.
  Verified end-to-end incl. the IDF 5.5.5 generation (AGENTS.md).
- **STM32**: no flash coredump (deliberate non-goal) — defmt panic
  backtrace over RTT suffices for the crash classes seen.

### 1.3 Dedicated live surfaces (never on the CCID wire)

- **netlog UDP :4567** — `firmware/esp32-ccid/src/netlog.rs`, compiled under
  `bench-net` (#219). Wired into the C3 WiFi mains (`wifi_ccid.rs`,
  `wifi_vpcd.rs` + `wifi_common.rs`) and the bring-up main; subnet-directed
  broadcast (bench-AP verified). Console arm doubles as the logger floor.
- **Reset-reason line on every boot** — `wifi_common.rs::log_reset_reason`
  (2026-10-28 wifi-vpcd reset hunt: the reboots under investigation write
  no coredump and no panic — the ROM reset reason is the only discriminator).
- **BLE NUS log console** — `ble` feature (`ble_log_queue` / `ble_logger` /
  `ble_debug` / `ble_console`); boards `m5stick-ble`, `c3-ccid-ble` (+`-diag`
  variant, #165). Advertising works on both boards (D-3); central connect +
  NUS-delivery bench proof is the remaining #66 item. Capture:
  `tests/hardware/ble/ble_log_capture.py`.
- **Diagnostic mains** — `pn7160-actdiag` (no CDC claim, console free): the
  bring-up pattern when the console claim itself perturbs the target.
- **GDB-over-JTAG** — nucula procedure in AGENTS.md (verified end-to-end).
- **STM32 defmt+RTT** — placement audit CLEAN (LOGGING_DOCTRINE addendum,
  2026-10-12: no records inside bit-cell loops / ISR / exchange internals).
  Open residual: defmt-rtt 0.4's blocking write (probe-rs unplug during
  power-on logging can stall the target) — the version bump is the real fix.
  RTT **Drop mode, never Block** (host-mode choice at attach; the repo pins
  no probe-rs config — the rule is doctrine, reasserted here).

### 1.4 Always-on floors

- **FWID boot banner** (`FWID <name> rev=<git> build=<ts>`, `build.rs`) —
  ends before the protocol driver claim; the one legal console use.
- **Clean-wire ceiling before claim** (#131) + **wire-silence regression**
  on BOTH wires: `test_wire_silence_after_banner` (nucula HIL) and the
  conformance battery's `quiet_bytes()` (the m5stick/GemPC twin, issues
  #339/#361 — a dropped port raises as bench fault, never passes as
  silence). The #133 checklist item "m5stick wire-silence HIL" is DONE
  here — see §3.6.
- **0xD0 counters** on both firmwares — the ~free always-on instrument.

## 2. What works best (tiered recommendation)

| Tier | Instruments | When |
|---|---|---|
| **0 — production, always-on** | 0xD0 counters (+ future trace ring §3.1); coredumps on crash; 0x02/0x6A on-demand identity | every serving build; time-sensitive paths |
| **1 — bring-up** | diagnostic builds (`actdiag` pattern); defmt+RTT (STM32, Drop mode, loop boundaries only); BLE NUS / netlog where the profile carries them | new silicon, new board, non-serving sessions |
| **2 — timing bisection** | NOTHING live: counters + trace ring + coredump + logic analyzer on the wire | the observation must not perturb the observed |
| **Never** | unsolicited bytes on a protocol wire (locked by the wire-silence test) | — |

Per-channel cost table (µs/line, stall modes): LOGGING_DOCTRINE.md §"defmt
is cheap, not free".

## 3. Decisions on the open options

### 3.1 Trace ring — YES, the next capability (and it takes `0xD5`)

A coredump is a frozen instant; the trace ring answers "**what sequence led
here**" (the #132-class question) at zero live-channel cost. Sketch, per the
#133 checklist plus the registry fact above:

- fixed ring, 64–128 slots × `(u32 timestamp, u8 event-id)`, drop-oldest;
- ring + event-ID enum live in `ccid-core` (no_std, host-testable); events
  from the paths we actually debug: T=1 re-frame/retry, PN7160 activation
  ladder steps, MFRC522 exchange turnaround, presence transitions;
- dump via **Escape 0xD5** (0xD4 is taken), payload = count + slots (LE);
  versioned per §3.2's shape from day one;
- host decoder in `host-tools/` consumed by the existing probes;
- one HIL proof, then wire into the probes' post-mortem output.

### 3.2 Versioned 0xD0 envelope — DEFER, with a trigger

The 28-byte layout is golden-test-locked and consumed positionally by the
HIL suite + probes; nothing needs v2 today. Decision: add the leading
version byte **at the first layout change**, not before — and make the
version constant + layout live in the future registry module (§3.5) so there
is exactly one place to be right. Retro-fitting a version byte with no
consumer is churn on a locked wire format.

### 3.3 "Logs into the coredump partition" (#118 option 3) — REJECTED as separate machinery

Checked against IDF 5.5.5: `CONFIG_ESP_COREDUMP_LOGS=y` (present in our
configs) captures the **espcoredump component's own strings**, not app logs
— so app-log-in-dump does not exist today. Rather than build a flash log
sink (wear, structure, retrieval = "coredump with extra steps" per
LOGGING_DOCTRINE's verdict), fold the need into §3.1: the ring is RAM-only
(zero wire/timing impact), dumped deterministically via 0xD5, and a crash
still freezes whatever the coredump captures. #118's remaining open item is
closed by this decision.

### 3.4 defmt policy — UNCHANGED

Audit result stands (clean hot loops); placement discipline + Drop-mode rule
stay the policy; the defmt-rtt version bump remains the tracked real fix.

### 3.5 Escape registry unification — the refactor plan (order-first)

One module in `ccid-core` owning the table: `(code, version, payload
layout, dispatch fn)`; reference-locked byte-exact tests for every payload
(stone-age rule: GREAT EXPECTATION). Per-firmware entries (STM32 `0x6A`
vendor-gated; ccid-service `0xD2`/`0xD3`/`0xD4` backend-conditional)
REGISTER at construction instead of hardcoding `if`-chains at three sites.
Pure refactor, host-testable, no wire bytes change — which is also why it
can land without a bench window (byte-identity is the acceptance test).

### 3.6 m5stick wire-silence — DONE on main (verified)

The #133 checklist drafted this as open; it is not. The conformance
battery's `Reader.quiet_bytes()` (issues #131/#133 cited in-tree,
hardened by #339/#361) asserts the GemPC wire delivers zero unsolicited
bytes post-handshake, with the dropped-port and flapping-port failure
classes correctly raised as bench faults. Both wires (nucula CDC,
m5stick UART0) are silence-locked; no further work.

## 4. Settled / non-goals

- No live-log reintroduction on any protocol wire (#131, settled — locked by
  the wire-silence test).
- No new transport channels: BLE and netlog stay as-is (bench profiles);
  both already have CI/build coverage (`c3-ccid-ble` leg, bench-net legs).
- No STM32 coredump-to-flash (defmt panic backtrace adequate; revisit only
  if a crash class demands it).

## 5. Cross-refs

`docs/LOGGING_DOCTRINE.md` (doctrine, cost tables, defmt audit) ·
`docs/DECISION_REGISTER.md` D-3 (BLE 0x0c wall closed) · AGENTS.md
(clean-wire doctrine, debug-channel matrix, dump-and-retrieve workflow) ·
issues: #66 (BLE, connect/NUS proof pending), #118 (debug channels off the
wire — status swept 2026-10-28), #133 (this doc), #101/#120/#156 (the
escapes that earned their codes), #131 (log_shim retirement).
