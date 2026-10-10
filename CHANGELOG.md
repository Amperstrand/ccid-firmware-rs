# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed — STM32F469 APDU/connect latency: 95.3ms → ~25-30ms APDU, 1507ms → ~0.5s connect

Bench decomposition (`tests/hardware/perf/benchmark_readers.py`, 300 iters, empty
SELECT on the ComSign eID T=1 card; CardMan 3121 reference 10.9ms/39ms) found three
systematic costs in the F469 USART driver (`smartcard.rs`), none of them jitter:

- **T=1 exchanges paid a flat +50ms end-of-block drain** — `transmit_raw` read the
  card's response with a 50ms inter-byte timeout and had no way to know the block
  was complete, so every APDU waited out the full timeout after the last LRC byte.
  The block length is fully determined by its 3-byte prologue (NAD PCB LEN, ISO
  7816-3 §11.3); `transmit_raw` now reads exactly `LEN+4` bytes for T=1 and
  terminates at the LRC (`t1_block_total_len` helper, host-tested). Non-T=1 keeps
  the legacy drain.
- **`read_atr`'s "50ms" inter-byte timeout was really ~0.7-1s** — it was a raw
  spin counter (`50 * 168_000` iterations) calibrated on an assumed 168
  cycles/iteration, but one iteration is a single volatile APB1 SR read plus
  branch (~15-25 cycles at 168MHz). This silent spin after the last ATR byte was
  the dominant term of the 1507ms connect. Now a `DwtWatchdog` real-time deadline.
- **`send_byte` quantized every TX byte to whole milliseconds** — the TC wait
  polled with `delay_ms(1)` granularity, adding up to ~1ms per byte over the
  ~0.93ms wire time at 11229 baud (~5-9ms per APDU, ~30ms at power-on). Now a
  tight poll bounded by a DWT deadline (same `byte_timeout_ms` budget).

Blocking architecture unchanged. Expected: APDU p50/p99 ~25-30ms (remaining gap
to CardMan is wire time at un-negotiated Di=1 — the card's TA1=0x18 offers Di=8;
real PPS with PPS1 is the follow-up lever), connect ~0.45-0.55s (residual: 267ms
of blind power-on settle delays + true 50ms ATR inter-byte wait + pcscd
negotiation exchanges). Not yet bench-verified (no hardware in this session).

### Fixed — PN7160 auto-activated ISO-DEP tags now detected end-to-end (issue #88 positive case, closes #105)

Bench root cause (nucula, 2026-10-10): the PN7160 AUTO-ACTIVATES a single ISO-DEP target during discovery — an NCI-sanctioned behavior (Linux kernel `nci_target_auto_activated()`, torvalds `net/nfc/nci/ntf.c`; ESPHome's pn7160 handles the same) — and emits `RF_INTF_ACTIVATED` instead of `RF_DISCOVER`, which the presence path discarded. The bench card was therefore reported absent by every firmware revision while sitting on the coil. Three coordinated changes in `pn7160-nci`:

- **Presence accepts auto-activations** — `wait_for_event` counts the activation NTF as a tag sighting; the NCI 2.0 payload (kernel `struct nci_rf_intf_activated_ntf` layout) yields the NFC-A UID from the tech params and the ATS from the activation params.
- **`power_on` never selects an activated interface** — the ATS is cached with the sighting and the ATR is built from it directly; APDUs ride the established connection. Bench-proven necessity: `RF_DISCOVER_SELECT` on an auto-activated interface WEDGES the PN7160 (I2C NACKs until power cycle).
- **Removal = RF_DEACTIVATE_NTF(reason RF-link-lost)** — an auto-activated tag leaving the field tears the RF link and the chip announces it; the driver clears presence immediately (the removal signal that never existed for discovery-resting tags). The empty-field re-arm now deactivates straight back to DISCOVERY (type 0x03, kernel `NCI_DEACTIVATE_TYPE_DISCOVERY`) — the manual `RF_DISCOVER` re-issue mutes the chip after ~11 cycles.

Bench-verified on the nucula (card on coil): 12/12 presence polls, `IccPowerOn` → ATR `3B 78 77 91 02 80 73 C8 21`, APDU round-trip `90 00`. The physical-removal leg is implemented and mock-tested; on-hardware removal verification still needs a hand. Also documents the #105 correction: deactivate-to-idle does NOT auto-restart discovery on this chip (the §5.2.5 claim in `9fd1626` was wrong).

### Changed — log-shim hardening: timestamps, visible truncation, HIL regression (issue #91)

Follow-up to the #91 fix, shaped by a best-practices pass (ESP-IDF's own
`I (ms)` line format; drop-oldest rings as the embedded standard):

- **Millisecond stamps on facade lines** — Rust `log` lines in the ring
  now carry an `esp_timer` stamp: `[WARN] (12345ms) target: message`
  (host-tested `format_stamped_record`). Sequence-diagnosis value the
  unstamped BLE-console format lacked; C-side `ESP_LOGx` lines keep
  IDF's own timestamped header.
- **Truncation is visible** — lines truncated by the 200-byte cap now
  end `~\n` (the marker rewrites the last payload byte at newline
  time, keeping the cap exact). A truncated log that looks whole is
  worse than one that admits it.
- **HIL regression test** — `test_log_shim_post_claim_output`
  (tests/hardware/nucula): DTR-low reset-and-capture asserts the
  pre-claim FWID, the ring-drained post-claim lines, and an LRC-valid
  GetSlotStatus answer amid the log text — locking the #91 behavior
  the way `test_escape_d1_snapshot_roundtrip` locks dump-and-retrieve.
  ESP32 host tests: 124 → 127.

### Fixed — USB-CDC log output restored after the driver claim (issue #91)

From the moment `UsbSerialDriver::new` claims the nucula's USB-Serial/JTAG peripheral, every log line logged afterwards was silently dropped — the PN7160 init ladder, `power_on failed: …` diagnostics, all serving-loop output. New `log_shim` module routes logs around the claim:

- **`esp_log_set_vprintf` sink for C-level `ESP_LOGx`** — the extern-C sink formats each chunk with `vsnprintf` (locally declared with the bindgen `va_list` alias; ABI round-trip bench-verified: riscv32 passes a pointer, xtensa passes the 12-byte va_list by value) and appends to a 4 KB static ring. IDF 5.5's text formatter calls the sink up to three times per line (header/message/`\n`), so the ring assembles lines from arbitrary chunks and only exposes newline-terminated ones.
- **`log` facade routing for Rust `log::warn!`/`error!`** — necessary addition to the researched design: esp-idf-svc 0.52's `EspLogger` writes via `fwrite` to newlib stdout, NOT through the vprintf sink, so the prescribed mechanism alone would leave the Rust diagnostics (the primary symptom) dead. `log_shim::init` replaces `EspLogger::initialize_default()` in the `pn7160-ccid` main; pre-claim lines still go to stdout (the FWID boot marker keeps landing on the console), post-claim they route into the same ring.
- **Ring semantics** — drop-oldest under overflow with a dropped-lines counter (drain emits an `=== N lines dropped ===` marker), 200-byte line cap, never blocks, never panics. Extracted as the pure host-tested `LogRing` (9 tests) mirroring the `ble_log_queue` pattern; the esp-idf shell is gated on `pn7160-ccid` + esp targets. ESP32 host tests: 100 → 109.
- **Drain points** — the serving loop drains the ring onto the claimed driver post-response and in the read-idle path (bounded burst; a failed write stops the pass). Interleaving with GemPC frames is safe: host-side parsers scan for SYNC-anchored LRC-validated frames (conformance-battery-proven; USB-CDC main only — the UART mains' clean-wire rule is untouched).
- **Bench-verified on the nucula** (rev 3e47a96, 2026-10-09): FWID lands on the console at boot (pre-claim path preserved), the post-claim lines (`USB-CDC ready`, the full PN7160 bring-up ladder) arrive ring-drained on the CDC, and framed GetSlotStatus/IccPowerOn get LRC-valid responses in the same byte stream as the log text.

### Changed — dump-and-retrieve verified on BOTH chips + hardened (issue #66 direction)

- **xtensa coredump configs** — `sdkconfig-xtensa.full`/`sdkconfig-xtensa-ble.full` previously had `CONFIG_ESP_COREDUMP_ENABLE_TO_NONE=y` (issue #64 only configured the C3); now `ENABLE_TO_FLASH` + ELF format everywhere. **M5Stick round-trip verified on hardware**: 0xD1 → echo + ack on the wire → silent flash coredump → reboot → decode (panic registers, task list, memory map).
- **Ack-before-panic flush** — the UART mains call `uart.flush_write()` between the 0xD1 response write and the snapshot panic: the UART write only queues into the driver's TX ring, and panicking immediately reset the peripheral before the ack clocked out (echo + dump present, ack missing). USB-CDC drains fast enough not to need it.
- **`sdkconfig.wallet-test`** (HIL `build_firmware()` fragment) now carries SILENT_REBOOT + coredump-to-flash, overriding `sdkconfig.defaults.esp32c3`'s PRINT_REBOOT (the hang mode) — the HIL firmware matches bench dump behavior.
- **HIL regression test** — `test_escape_d1_snapshot_roundtrip` (tests/hardware/nucula): flash `pn7160-ccid` via the verified-boot ladder → erase coredump partition → Escape 0xD1 → assert echo+ack on the wire → assert dump written → decode (panic reason, abort marker, crashed task `main`) → clean erase. Tolerates the IDF 5.5.1 esp_coredump thread-printer crash on newer gdb `LWP` ids (documented in AGENTS.md).
- **`esp32_coredump.py trigger` hardened** — waits for boot/banner traffic to stop before sending (a frame fired mid-boot is processed unpredictably), and reads until the echo+ack pair instead of a fixed 256-byte window (netlog on UART0 buries the ack in LED noise).
- **CI espup crosstool pin** — `espup install -c 14.2.0_20241119`: espup defaults to the latest crosstool whose bin dir precedes embuild's copy in PATH, and the `esp-16.2.0_20260914` release (2026-10-09) failed every xtensa matrix entry at IDF's `tool_version_check`. Also added a `c3-nucula-ccid` matrix entry so the bench-shipped reader firmware has target-compile coverage (previously host-tests only).
- **Clean-wire rule for the UART mains (pcscd interop)** — libccidtwin's parser rejects any non-CCID bytes interleaved with GemPC frames: even WARN-level "power_on failed" lines made pcscd error-loop. The mfrc522 main now sets `log::set_max_level(Off)` in non-`ble` builds (UART0 = CCID frames only; diagnostics via Escape 0xD0/0xD1 or a `ble` build) — closing the pre-existing comment/code mismatch where the comment claimed full suppression but the code set Info. Bench-verified: GemPCTwin enumerates and polls clean under pcscd alongside all six other readers.

### Added — dump-and-retrieve snapshot debugging (issue #66 direction)

Preferred debug workflow for time-sensitive paths (NFC card I/O, CCID wire timing): run the firmware undisturbed, then capture state post-mortem — no live debug channel means no timing perturbation and no wire contention.

- **CCID Escape 0xD1 diagnostic snapshot** — the firmware acks the escape, then panics with `escape 0xD1: diagnostic snapshot requested` AFTER the ack is on the wire, so the panic handler writes a flash coredump of the live state (issue #64 mechanism). Wired into all three ESP32 serve loops (USB-CDC `pn7160-ccid`, UART mfrc522/pn532 mains); the flag consumes exactly once so a retried 0xD1 is a fresh snapshot. Host-tested. **Hardware round-trip verified on the nucula** (IDF v5.5.1): trigger → ack → silent dump → reboot → decode yields the panic reason, a symbolized backtrace through `abort_internal` → `panic_abort`, and every task's state; the reader serves CCID again after the reboot.
- **`CONFIG_ESP_SYSTEM_PANIC_SILENT_REBOOT=y` in all full sdkconfigs** — the default PRINT_REBOOT mode hangs on the nucula: the panic handler's print stage blocks on the C3's USB-Serial/JTAG console once `UsbSerialDriver` has claimed the peripheral (JTAG-verified: the snapshot reached `panic_abort`'s unimp trap, then no backtrace, no coredump, no reboot). Silent mode dumps and reboots without printing — which is the dump-and-retrieve philosophy anyway (and on UART-console boards panic prints would corrupt the CCID wire).
- **`tests/hardware/esp32_coredump.py`** — one-command trigger/retrieve/erase for the workflow: sends the GemPC-framed 0xD1 over raw serial, reads the coredump partition (0x3F0000), and decodes it with the auto-discovered embuild esp-idf + gdb-multiarch (the manually documented decode procedure had rotted on a stale v5.2.3 IDF path — this was causing "no such file" decodes after the v5.5.1 move).
- **AGENTS.md "Crash Dumps & Snapshot Debugging"** — the workflow, its limits (a dump is a frozen instant, not a sequence), and the complementary flight-recorder channels (Escape 0xD0 counters, actdiag, experimental BLE console, GDB-over-JTAG).

### Added — BLE debug console (issue #66) — EXPERIMENTAL, xtensa bench only

Firmware logs ride BLE GATT notifications (Nordic UART Service lookalike — service `6E400001-…`, notify characteristic `6E400003-…`) so the CCID wire carries protocol frames only. **Parked by owner decision after bench bring-up**: dump-and-retrieve is the preferred style; the C3 path is blocked by a controller quirk (below). Code stays feature-gated, host-tested, zero cost in non-`ble` builds.

- **`ble_console` facade** — one `BleConsole::init(modem, silence_c_logs)` call per main: installs the `log` sink, optionally silences C-level ESP_LOGx at runtime, brings up BtDriver + GATT server + advertising; `drain()` from the serve loop. Wired into the MFRC522 main (xtensa + riscv32) and the `pn7160-ccid` USB-CDC main (nucula — previously console and CCID shared the CDC port). Firmware-side bring-up failures now print step-tagged `ble:` diagnostics on the console (the BLE sink may be what's failing).
- **`ble_log_queue` module** — the queue logic (drop-oldest ring, dropped counter, attach banner, 200-byte line truncation) extracted as a pure, always-compiled module with 7 host tests, mirroring the `ccid_serial_server` extraction pattern. ESP32 host tests: 81 → 89.
- **Logger-ordering bug fixed (latent)** — the Rust `log` crate accepts one global logger, first installer wins; the old M5Stick flow called `EspLogger::initialize_default()` before `BleLogger::install()`, so the BLE queue stayed empty forever and logs kept hitting the console. Under `ble`, mains no longer call `EspLogger::initialize_default()`; `BleConsole::init` owns the ordering. Stub `drain` signature also fixed (was `&()`, now the stub `BleDebugServer`).
- **NUS UUIDs** — the debug service switched from a custom UUID to the Nordic UART Service convention (ecosystem standard — embassy and h2zero's NimBLEStream both build on it), so stock centrals and terminals attach without custom tooling.
- **Canonical Bluedroid adv chaining** — adv-data write completes → scan-rsp write → scan-rsp completion is the only safe point to `start_advertising` (mirrors the IDF gatt_server example; the 31-byte ADV budget carries flags + UUID + TX power, the name goes to the scan response).
- **`sdkconfig-xtensa-ble.full`** — committed BT-enabled bench config replacing the orphaned `sdkconfig.defaults.ble` (nothing referenced it, and its classic-chip `BTDM_CTRL_MODE_*` symbols are invalid on C3). **`sdkconfig-c3-ble.full`** is committed with a header warning but unused: the full Bluedroid chain completes on C3, yet the controller deterministically rejects LE Set Advertising Parameters with Command Disallowed (0x0c) — BLE_42-on/BLE_50-off, the adv/scan-rsp split, and canonical event chaining were each necessary (each fixed an earlier failure) but not sufficient. No `c3-ccid-ble` build.sh board until root-caused.
- **build.sh: `m5stick-ble` board entry** — pairs the `ble` cargo feature with its BT sdkconfig (a mismatched pair fails at compile time — deliberately loud). The sdkconfig stamp is now keyed per (triple, profile) instead of per board: boards sharing a target share one esp-idf-sys cmake cache, and the old per-board stamps would let a stale non-BT cache silently serve a `-ble` build after switching boards. This also closes the original issue's "cmake cache doesn't pick up sdkconfig BT changes" blocker.
- **`tests/hardware/ble/ble_log_capture.py`** — bleak-based NUS central for the bench (scan by name prefix, subscribe, timestamped follow, optional RX writes), plus a README for testers. Bench status: the M5Stick advertises correctly (connectable ADV_IND + SCAN_RSP confirmed at HCI level via btmon); bench-central connects are currently blocked by a host-side BlueZ quirk (stale BR/EDR device-object resolution → classic page instead of a GATT link; workaround documented in AGENTS.md — remove the cached device while it exists / restart bluetoothd, or use a phone with nRF Connect).
- **FWID stance** — in `ble` builds the FWID banner goes to the BLE ring, not the console; HIL/labgrid verification stays on standard builds (documented in AGENTS.md "BLE Debug Console").

### Changed — nucula CCID serving core extracted + host tests

- **`ccid_serial_server` module** — the `pn7160-ccid` USB-CDC main loop's serving logic (frame echo, response framing via `ccid-transport-serial`, interval-gated card polling) extracted into a host-testable module with 8 unit tests: round-trip echo + framed response, LRC validity, corrupt-frame recovery, garbage tolerance, poll gating on GetSlotStatus and read-idle, first-poll-after-interval semantics, and ATR delivery through IccPowerOn. Wire behavior preserved: parse errors remain silently dropped (the divergence from the NAK-ing UART main is documented in-module); the echo now carries only the parsed frame bytes (leading garbage skipped); the unreachable manual overflow-NAK path (the frame parser rejects oversized payloads at header time) is removed together with its dead `record_nak` call.
- **`CcidHandler::driver_mut()`** — mutable driver accessor for health probing/recovery and test instrumentation.
- **`MockNfcDriver`** — presence-poll counter; `poll_card_presence` now routes through `is_card_present()` so presence polls are observable in tests.
- ESP32 host tests: 72 → 80.

### Added — C3/nucula CI coverage + fresh-runner partition injection (issue #70)

- **`c3-nucula-bringup` matrix entry in `esp32-build`** — the riscv32imc-esp-espidf target now builds in CI on the **bench `sdkconfig.full` flow** (coredump partition, USB-JTAG console), not a defaults approximation, so CI validates the config the bench actually ships. Toolchain per esp-idf-sys's own CI convention: nightly + `rust-src`, `ldproxy`, zero apt-only deps (embuild self-provisions cmake/ninja/python/esp-clang under `target/.embuild`, which the existing target-dir cache already covers). Matrix entries are now parameterized (`toolchain`/`profile`/`target`/`sdkconfig`), with espup/espflash conditional on the xtensa variants. Debug profile = bench parity.
- **Fresh-runner partition-table injection via `ESP_IDF_GLOB_*`** (esp-idf-sys BUILD-OPTIONS mechanism; copies run inside the build script before cmake configure): `sdkconfig.full` now references `partitions-ota.csv` by its real name and `build.sh` exports `ESP_IDF_GLOB_PARTCSV_BASE`/`_1` — replacing the manual post-configure out-dir copies entirely. **Docker fresh-runner simulation verified**: pristine ubuntu:24.04 + rustup nightly only → cold build 4m17s, generated sdkconfig carries `PARTITION_TABLE_CUSTOM=y`, and the produced `partition-table.bin` is **byte-identical** to the hardware-verified bench build. Bench re-verified via `build.sh c3` against the shared target dir (same identical table).
- **`backend-pn7160` host tests now run in CI** (`esp32-host-test` gained a second invocation; previously its 53 tests — bring-up driver, NCI adapter, health-check accessors — never ran in CI).
- **ESP-IDF pin stays `tag:v5.2.4` — pin-down attempt reverted with findings** (issue #70 research): the manifest said v5.2.4 while every bench artifact ran a stale-provisioned v5.2.3, so aligning down looked free. A fresh-runner CI run proved otherwise: embuild's tool provisioning for a fresh v5.2.3 install (ninja 1.11.1) does not match v5.2.3's own `idf_tools.py` expectations (resolves ninja 1.12.1) — cmake configure fails. v5.2.4 provisioning is CI-proven since #73. The bench's v5.2.3 `.embuild` tree is a long-accumulated artifact that happens to satisfy both; do NOT delete it casually. A deliberate bench migration to v5.2.4 (fresh clone, re-normalized sdkconfig, reflash + reverify) is the correct future alignment — deferred as its own task.
- Research notes: esp-idf-sys was archived 2026-09-19 into the `esp-rs/esp-idf` monorepo (no action — we pin 0.37.x from crates.io). esp-idf-sys's own CI uses prebuilt ldproxy zips instead of `cargo install`; deferred as a micro-optimization since `~/.cargo/bin` is already cached.

### Changed — CI build pipeline speed + DRY (issue #69)

- **esp32-build target-dir caching** — the esp32-build matrix (~8 min/job) restored `~/.espressif` but never the cargo target dir, where esp-idf-sys compiles the whole ESP-IDF C framework + Rust std (`-Z build-std`) — the dominant ~80% of build time recompiled on every run, on 3 separate runners. The espressif cache block now also caches the repo-root `target/` (esp32-ccid is a root-workspace member, so artifacts land there — the same dir `s_check_sdkconfig` reads — not `firmware/esp32-ccid/target`), keyed per matrix variant on the esp32 manifest + workspace lock, with a restore-key chain back to the v2 tools-only cache. Warm-path expectation: ~8 → ~2–3 min per job. Budget note in-workflow: 3 variants ≈ 4–6 GB compressed of GitHub's 10 GB/repo LRU cache.
- **Concurrency cancellation** — workflow-level `concurrency` (`cancel-in-progress: true`): a superseded push to the same ref cancels the in-flight run instead of queueing a full obsolete matrix run behind the newest commit.
- **Path-filtered job groups** — new `changes` job (dorny/paths-filter@v3) guards the groups: stm32-build/lint/test on `firmware/ccid-firmware/**`, `crates/**`, `host-tools/**` (its tests run under `cargo test --workspace`), `vendor/**`, root manifest/lock; esp32-build/esp32-host-test on `firmware/esp32-ccid/**`, `crates/**`, root manifest/lock. stm32-lint additionally triggers on a `lint` filter (Rust sources of any workspace member, since `cargo fmt --check` is workspace-wide, plus the shell/python helpers it syntax-checks) — closing the false-skip hole where an esp32-only `.rs` change would not run fmt. All jobs stay defined: skipped jobs report success, so required status checks keep working.
- **Bench build script** — new `firmware/esp32-ccid/build.sh <c3|m5stick|m5atom> [--flash <port>] [--dry-run]` encoding the verified ai-legion runbook incantations (per-board toolchain/sdkconfig/target/features/flash parameters, `ESP_IDF_GLOB_*` partition-CSV injection, NUCULA WiFi credential passthrough); wired into the stm32-lint `bash -n` gate. Bench flow only — the defaults flow + pcscd testing remain in `flash_and_test.sh`.
- C3 (nucula) CI matrix coverage deliberately deferred to #70 (fresh-runner `partitions.csv` resolution unverified).

### Verified — GDB over built-in USB-JTAG (issue #64, second half)

- **Live GDB debugging proven on ai-legion** — Espressif's `openocd-esp32` (Debian's openocd 0.12 lacks `esp32c3.cfg`/`esp_usb_jtag.cfg`) + `gdb-multiarch` as client. A hardware breakpoint on `i2c_master_cmd_begin` hit within one 5 s health-probe cycle with a fully symbolized backtrace (`i2c_master_cmd_begin` → `I2cDriver::write (addr=0x28)` → `EspPn7160Transport::probe` → `pn7160_bringup::run` → `main`) and live argument values; the target resumed cleanly and the health loop continued undisturbed. With this, both halves of issue #64 are done: coredump-to-flash (merged in #68) and real-time GDB. Verified procedure recorded in `AGENTS.md` ("Coredump decode" section tail).

### Added — nucula crash forensics + PN7160 health monitoring (issues #64, #63)

- **Coredump-to-flash (issue #64)** — new `coredump` data partition at `0x3F0000..0x400000` (last 64 KB of the 4 MB flash, past both OTA slots) in `firmware/esp32-ccid/partitions-ota.csv`, plus `CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH=y` and `CONFIG_ESP_COREDUMP_DATA_FORMAT_ELF=y` in `sdkconfig.full`. Verified end-to-end on the nucula board: deliberate panic → flash write → `esptool read-flash 0x3F0000` → `espcoredump.py info_corefile` decode via gdb-multiarch, yielding the panic reason and symbolized frames (`panic_abort` → `esp_system_abort` → `std::sys::pal::unix::abort_internal`). Decode procedure documented in `AGENTS.md` ("Coredump decode"). Note: the partition table (`0x8000`) must be reflashed alongside the app whenever `partitions-ota.csv` changes.
- **PN7160 periodic I2C health check (issue #63)** — the `pn7160-bringup` main no longer goes permanently deaf after a failed init ladder. It probes I2C `0x28` every 5 s and logs each result (`health[N]: PN7160 @0x28 no-ack: ...`), so a chip that returns after a power cycle is caught immediately: the first ACK logs loudly, runs the bus scan + NCI init ladder, and on success enters the card heartbeat. A failed ladder VEN-re-cycles (verdict-timed) and resumes probing; after a successful init a 60 s probe watches for mid-session chip death and drops back to the probe loop. `EspPn7160Transport::probe()` is the new single-address health primitive; `Pn7160NfcDriver::{transport_mut, into_transport}` expose the link for probing/recovery. Transport-construction failure now logs every 150 s instead of sitting silently (bolty-rs B5 pattern).
- **Host tests** — 2 new tests for the adapter accessors (`transport_mut` non-consuming reach-through, `into_transport` recovery with wire history intact), run under `--features backend-pn7160`.
- **Docs** — `AGENTS.md` gains the nucula build flow section: the env var is `ESP_IDF_SDKCONFIG` (plain `SDKCONFIG` is silently ignored and the build silently falls back to stale cached defaults — including stale absolute paths from other working copies), the cmake-cache + `partitions.csv` copy ritual after sdkconfig changes, `elf2image` before `write-flash`, and the coredump read/decode commands.
- **fmt** — workspace `cargo fmt` (Rust 1.92, the CI pin) applied; the `stm32-lint` format gate was red on `c3-port` since the pn7160-nci extraction commits.

### Added — nucula CCID campaign archive (docs)

- **`docs/nucula-campaign/`** — archived working documents from the October 2026 nucula bring-up campaign (the work that landed the ESP32-C3/PN7160 support on `main`): the campaign runbook with its append-only status log (hardware setup, build rules, phase plan, full debugging history), the PN7160/NCI/ISO-14443 spec citation map behind the `pn7160-nci` annotations, and its quick-reference companion. Preserved for future bring-ups; historical record, not maintained docs. The NXP `linux_libnfc-nci` reference clone is deliberately not vendored.

### Reversed — amp-embedded-common dissolved (necessity audit)

- **T13 consumption reversed** — the existential necessity audit (`.omo/evidence/amp-necessity-audit.md`, 2026-08-31) found the repo served exactly one legal consumer (this one) and its crates were in-house parallel discovery, not library-sized work. All three rev-pinned git dependencies are removed and the modules restored in-repo behind the same paths — **zero call-site changes**: `dwt_watchdog` (329 lines + 14 tests) back to `firmware/ccid-firmware/src/dwt_watchdog.rs` via `pub mod dwt_watchdog;`, `Diagnostics` (28-byte frozen wire format + 12 tests) back to `crates/ccid-core/src/diagnostics.rs`, `InitRecoveryTracker` back inline in `firmware/esp32-ccid/src/mfrc522_driver.rs`.
- **One test rescued before archival** — the byte-exact golden serialization test written in `amp-diagnostics` (pinning the frozen CCID Escape 0xD0 wire layout; never previously in this repo) is ported verbatim into the restored `diagnostics.rs` tests module.
- **USB-PHY reset sequences stay inline, now deliberately** — the `TODO(amp-recovery)` markers are replaced by the audit verdict (kept inline; `amp-recovery`'s `usb_phy` duplicated what the HAL/vendored driver stack already does and had zero consumers).
- **The workflow survives** — the reusable `rust-embedded.yml` CI workflow (the audit's one KEEP) was relocated to org level at `Amperstrand/.github` in Phase A; bolty-rs consumes it from there. The `Amperstrand/amp-embedded-common` repo itself is archived (read-only).
- Workspace host-test counts: ccid-core 21 → 33, ccid-firmware 61 → 75, esp32-ccid unchanged at 72 (its 4 tracker tests never left); total 227 → 253.

### Changed — amp-embedded-common consumption (T13)

- **`dwt_watchdog` module removed — consumed from `amp-dwt-watchdog`** (amp-embedded-common, rev-pinned `67ceee1`). The 329-line local module is deleted; `ccid-firmware-rs` re-exports the shared crate as `ccid_firmware_rs::dwt_watchdog` so import sites (`smartcard.rs`, `main.rs`) are unchanged. The 14 host tests moved to the canonical crate (verified green at the pinned rev).
- **`Diagnostics` consumed from `amp-diagnostics`** (same repo/rev). `crates/ccid-core/src/diagnostics.rs` is now a re-export (`pub use amp_diagnostics::Diagnostics;`) — the `ccid_core::Diagnostics` path used by firmware and esp32 code is unchanged. The 11 tests plus a new byte-exact golden serialization test live in the canonical crate.
- **`InitRecoveryTracker` consumed from `amp-recovery`** — the local struct in `firmware/esp32-ccid/src/mfrc522_driver.rs` is deleted and imported from the shared crate (no feature flags required). The 4 driver tests remain in place, unchanged.
- **USB PHY reset stays inline** — `amp-recovery`'s `reset_usb_otg_phy()` could not be consumed: its `stm32f4`/`stm32f7` features do not compile at rev `67ceee1` (references `stm32f4xx-hal`/`stm32f7xx-hal`/`cortex-m` without declaring them as dependencies — E0433). Both inline blocks in `firmware/ccid-firmware/src/main.rs` remain authoritative, marked `TODO(amp-recovery)` until upstream fixes the feature wiring.
- Workspace host-test counts shift accordingly: ccid-core 32 → 21, ccid-firmware 74 → 60 (tests moved to the canonical crates); esp32-ccid unchanged at 72.

### Changed — Shared MFRC522 fork (issue #19)

- **Removed `vendor/mfrc522/` (~1,500 lines)** — the divergent local MFRC522 driver copy is gone. `esp32-ccid` now consumes the canonical `Amperstrand/mfrc522-rs` fork (`ai-experiments` rev `e9ced1e`, git dependency) — the single source shared with bolty-rs, eliminating the two-repo vendor drift.
- The unified fork carries the union of both former copies: the 5000-iteration software timeout caps on the MFAuthent/transceive wait loops (hang protection previously only in bolty-rs), `std`-gated unit tests, and repaired eh02 mock-test expectations.
- `mfrc522-pcd` bumped `ea6d381` → `0835d09` (bolty-rs) so its transitive `mfrc522` dependency resolves to the same git rev as the direct dependency — exactly one `mfrc522` package in the graph.
- Removed the `[patch."https://github.com/Amperstrand/mfrc522-rs.git"]` table entry; `vendor/iso14443-rs` and `vendor/synopsys-usb-otg` remain vendored.
- Removed the vestigial tracked `firmware/esp32-ccid/Cargo.lock` (unused by cargo — the workspace-root lock governs; it still referenced the deleted vendor path).

### Changed — Shared ISO 14443 fork (issue #6)

- **Removed `vendor/iso14443-rs/`** — the local ISO 14443 crate copy is gone. `esp32-ccid` now consumes the canonical `Amperstrand/iso14443-rs` fork (`ai-experiments` branch, git dependency) — the single source shared with bolty-rs, eliminating the two-repo vendor drift.
- The unified fork carries the APIs needed for MFRC522 hardware workarounds: `PcdSession` (session-based ISO-DEP lifecycle), `try_set_timeout_ms` (configurable timeout), `set_fsc` (frame size capping for 64-byte FIFO), and `set_base_fwt_ms` (base frame waiting time).
- Both direct and transitive dependencies resolve to the same `ai-experiments` branch — exactly one `iso14443` package in the graph.
- Removed the `[patch."https://github.com/Amperstrand/iso14443-rs.git"]` table entry; `vendor/synopsys-usb-otg` remains vendored.
- Removed the CI `iso14443-host-test` job (tests now run via the canonical fork).

### Added — Sibling-repo improvement pass (issues #28–#32)

- **DWT cycle counter watchdog (#28)** — new `dwt_watchdog` module in `firmware/ccid-firmware/src/dwt_watchdog.rs`. Provides accurate wall-clock timeouts on Cortex-M3+ using the DWT CYCCNT register. Replaces iteration-based polling in F469 `SmartcardUart::receive_byte_timeout()`. 14 host-side unit tests. Pattern sourced from gm65-scanner (commit 1d7fddc).
- **Diagnostics struct (#29)** — new `Diagnostics` struct in `crates/ccid-core/src/diagnostics.rs`. Tracks runtime counters (apdu_tx_count, apdu_rx_count, nak_count, error_count, reinit_count, card_present, uptime_ticks). Serializes to fixed 28-byte little-endian layout for CCID Escape vendor command. 11 unit tests. no_std, zero-dep.
- **Escape 0xD0 diagnostic query (#29)** — vendor-neutral CCID Escape path: host sends `PC_TO_RDR_ESCAPE` with payload `[0xD0]`, firmware returns serialized Diagnostics struct. Works on ALL reader profiles (Cherry, CT30, K30, F746) — not restricted to Gemalto vendor. Implemented for both STM32 (`ccid_core.rs`) and ESP32 (`ccid_handler.rs`).
- **ESP32 diagnostics counters (#29)** — wired Diagnostics into ESP32 `CcidHandler`: apdu_tx_count on XfrBlock, apdu_rx_count on response, error_count on failures, card_present from NFC driver, nak_count from serial NAK path.
- **SmartcardConfig struct (#31)** — replaced 10 hardcoded `const SC_*` values in `smartcard.rs:30-39` with a configurable `SmartcardConfig` struct in `smartcard_common.rs`. Values unchanged (characterization test verifies). Enables per-deployment tuning. Pattern from gm65-scanner `ScannerConfig`.
- **Self-healing SmartcardWrapper (#30)** — STM32 `SmartcardWrapper` now re-initializes the smartcard peripheral after 3 consecutive APDU failures, increments `reinit_count`, and continues operation instead of returning errors indefinitely. Added `fn diagnostics()` default method to `SmartcardDriver` trait.
- **Self-healing MFRC522 driver (#30)** — ESP32 `Mfrc522NfcDriver` now performs full re-init after `REINIT_THRESHOLD=3` consecutive init failures, tracks `reinit_count`. Removed permanent error-LED halt pattern. Added `fn reinit_count()` default method to `NfcDriver` trait.
- **Labgrid HIL test harness (#32)** — new `tests/hardware/labgrid/` directory with pytest-based HIL tests. SSH-based fixtures wrap st-flash, lsusb, pcsc_scan, pyscard on the remote STM32 host. 6 HIL tests: USB enumeration, pcscd detection, ATR verification, APDU round-trip (SELECT MF, GET CHALLENGE), pinpad capability. All pass on real hardware.

### Fixed
- **CI clippy fix (#25)** — `default` features now include `stm32f469` MCU target. Previously default only had device profile, causing F469 clippy to fail with unresolved `pac`, `UsbBus`, `CcidClass`, `smartcard_wrapper` symbols.
- **HAL fork pin bump (#23)** — stm32f4xx-hal bumped from `789e5e8` to `05d999d` for PLLSAI P/Q divider preservation fixes.
- **USB OTG FS PHY reset (#22)** — added PHY reset sequence (clock disable/enable, peripheral reset, core soft reset, GCCFG power-cycle) for stm32f469 target. Fixes USB not re-enumerating after `st-flash` soft reset. **Hardware verified** on STM32F469I-DISCO at 192.168.13.208: Cherry ST-2xxx enumerates correctly after st-flash soft reset.
- **ESP32 stack overflow (#21)** — fixed incorrect `sdkconfig.defaults` option name: `CONFIG_ESP_MAIN_TASK_STACK_SIZE` → `CONFIG_MAIN_TASK_STACK_SIZE=12288`. Previous 32KB setting was never applied due to wrong option name.

### Hardware Verification (August 2026)
- **STM32F469I-DISCO** at 192.168.13.208 (ST-LINK/V2.1 serial 066FFF515786534867184152):
  - Cherry SmartTerminal ST-2xxx (VID:PID 046A:003E) enumerates after st-flash ✅
  - pcscd detects reader, reads ComSign eID ATR `3B D5 18 FF 81 91 FE 1F C3 80 73 C8 21 10 0A` ✅
  - APDU round-trip via pyscard: SELECT MF → SW 6A 86, GET CHALLENGE → SW 6E 00 ✅
  - 6/6 HIL tests pass (18.9s total) ✅
  - USB PHY reset (issue #22) confirmed working on real hardware ✅
  - All Wave 1–3 features (DWT, Diagnostics, SmartcardConfig, self-healing, Escape 0xD0) running on-device ✅

### Test count
- Workspace host tests: 263 (was ~218 baseline pre-improvement-pass)
- ESP32 host tests: 65 (includes iso14443 crate tests via git dependency)
- HIL tests: 6 (all pass on real hardware)

## [0.1.1] - 2026-05-03

### Added
- **Shared CCID architecture** — four new shared crates under `crates/`:
  - `ccid-protocol` — protocol types, constants, ATR parsing (moved from root)
  - `card-interface` — card frontend trait and types (no_std)
  - `ccid-core` — CCID response builders, PPS validation, parameter lookup (21 tests)
  - `ccid-transport-serial` — GemPC Twin serial CCID framing (25 tests)
- **Workspace directory reorganization** (Phase 6):
  - `firmware/ccid-firmware/` — STM32 USB CCID firmware (moved from root)
  - `firmware/esp32-ccid/` — ESP32 serial CCID firmware (moved from `esp32-ccid/`)
  - `crates/ccid-protocol/` — shared protocol (moved from `ccid-protocol/`)
  - Root `Cargo.toml` is now a pure workspace manifest with profiles and patches
- **ESP32 ccid_handler refactor** — uses shared `ccid_core` response builders, PPS validation, parameter lookup instead of local duplicates
- **PresenceState** — defined once in `card-interface`, shared across STM32 and ESP32
- **CI** — F746 build matrix entries now include profile feature flag
- 337 total tests across all workspace members

### Changed
- F746 card clock increased from 1 MHz to 5 MHz (ISO 7816 maximum), 2x APDU throughput
- All 5 clippy warnings in STM32 firmware eliminated (semantic no-ops)
- `BUILDING.md`, `README.md`, `Dockerfile`, CI workflow updated for new directory layout

### Fixed
- F746 performance: card clock 1->5 MHz (hardware verified at 74.4ms avg round-trip)
- CI F746 build entries missing profile feature flag
- `replay_seedkeeper_full_session` test: expected byte corrected for ATR-derived TB3 params

### Hardware Verification (May 2026)
- STM32F746-DISCO (Cherry ST-2xxx USB CCID): 74.4ms avg, ComSign eID T=1 contact card
- Both F746 and F469 firmware builds verified clean
- ESP32 hardware testing pending (M5Stack Atom disconnected)

## [0.1.0] - 2026-04-24

### Added
- **ESP32 NFC CCID firmware integrated into main branch** (`esp32-ccid/`)
  - MFRC522 NFC backend over I2C (M5Stack Atom Matrix) — primary NFC path
  - PN532 NFC backend over SPI — secondary, remains supported
  - GemPC Twin serial CCID protocol over UART0 (115200 8N2)
  - 75 host-side unit tests covering serial framing, CCID parsing, NFC logic, LED patterns
  - WS2812 LED diagnostic patterns (init, ready, card present, TxRx, error)
  - BLE debug logger (optional, behind `backend-mfrc522` feature)
- **Vendored patched dependencies** under `vendor/`
  - `vendor/iso14443-rs/` — patched ISO 14443 protocol crate (PcdSession, try_set_timeout_ms, set_fsc, set_base_fwt_ms)
  - `vendor/mfrc522/` — patched MFRC522 driver crate
- **CI coverage for both products**: STM32, ESP32, and iso14443 host-test jobs
- **esp-idf-svc 0.52.1** from crates.io with ESP-IDF 5.2.4 pin

### Hardware Verification (2026-04-24)
- **ESP32 + MFRC522 (GemPC Twin serial):** pcscd detects reader, NFC card responds
  - Card: NXP P71 SmartMX3 P71D320 JCOP4 JavaCard
  - ATR: `3B 85 80 01 80 73 C8 21 10 0E` (TCK correct)
  - Reader: `GemPCTwin serial 00 00`
- **STM32 + Specter DIY Shield (Cherry ST-2xxx USB CCID):** pcscd detects reader, contact card responds
  - Card: ComSign eID (T=1, IFSC=254)
  - ATR: `3B D5 18 FF 81 91 FE 1F C3 80 73 C8 21 10 0A` (TCK correct)
  - Reader: `Cherry GmbH SmartTerminal ST-2xxx (ST2XXX-001) 02 00`
- Both readers verified simultaneously on the same host
- All host tests pass: STM32 82/82, ESP32 75/75, iso14443 52/52

### Changed
- `.gitignore`: `vendor/**/target/` instead of blanket `esp32-ccid/vendor/`
- `esp32-ccid/src/led.rs`: Host-build gating with `#[cfg(all(target_arch, feature))]`
- `esp32-ccid/src/ble_debug.rs`, `ble_logger.rs`: Poison-recovering lock helpers
- `README.md`, `BUILDING.md`, `esp32-ccid/README.md`: Two-product documentation

### Notes
- `esp32-serial-ccid` branch merged into `main` via fast-forward + rebase
- Branch `esp32-serial-ccid` deleted after successful merge
- Known FTDI FT232 wedge bug: espflash DTR/RTS toggles require physical USB replug after flash

## [0.0.9] - 2026-03-17

### Added
- **CCID Spec Compliance Documentation**
  - `docs/CCID_SPEC_AUDIT.md`: Comprehensive spec compliance audit
  - `docs/AUDIT_PLAN.md`: Structured comparison of spec vs osmo vs our implementation
  - All CCID command handlers now include spec citations in doc comments

### Changed
- **README.md**: Added CCID compliance section with feature comparison table
- **IccPowerOn**: Now validates dwLength==0 per CCID §6.1.1
- **Spec citations**: All command handlers reference CCID Rev 1.1 spec sections
- **Compliance rating**: Improved from 95% to 98%+

### Fixed
- dwLength validation in IccPowerOn per CCID spec requirement

### Notes
- **Embassy Migration**: Documented requirements for async runtime migration
- **osmo Comparison**: Documented where we exceed osmo (PIN verify/modify) vs match (stubs)
- **Future Work**: Identified what would need to change for multi-slot, async, or TPDU level support

---

## [Unreleased]

### Added
- Added osmo-ccid-firmware as git submodule at `reference/osmo-ccid-firmware/` for protocol reference
- Added `docs/SPECIFICATIONS.md` with links to official CCID, ISO 7816, and PC/SC specifications
- Added compliance review documentation for stub commands (Escape, T0APDU, Mechanical, Abort)

### Changed
- **Profile naming refactored** to align with CCID project conventions:
  - `profile-cherry-st2100` → `profile-cherry-smartterminal-st2xxx`
  - `profile-gemalto-plain` → `profile-gemalto-idbridge-ct30`
  - `profile-gemalto-pinpad` → `profile-gemalto-idbridge-k30`
- **CRITICAL FIX**: Gemalto IDBridge K30 profile no longer falsely claims PIN pad and LCD capabilities
  - Real K30 has bPINSupport=0x00, wLcdLayout=0x0000 (no PIN, no LCD)
  - K30 uses TPDU level (0x00010230), not Short APDU (0x00020472)
  - For PIN pad support, use profile-cherry-smartterminal-st2xxx (the only PIN-capable profile)
- All profiles now match CCID reference files exactly (reference/CCID/readers/*.txt)
- Fixed GEMALTO_K30_DWFEATURES constant: 0x00010230 (was incorrectly 0x00020472)
- Fixed dwFeatures decomposition tests to match actual reference values
- Added upstream CCID project as git submodule for authoritative device reference

### Compliance Notes
- Voltage support verified correct per profile (Cherry: 0x01, Gemalto: 0x07)
- Stub commands (Escape, T0APDU, Mechanical) intentionally return CMD_NOT_SUPPORTED
- Abort command returns success (matches osmo-ccid-firmware behavior for single-slot)
- T=1 prepare_rx() is intentional trait default, not a bug
- Time extension handling requires async architecture (future enhancement)

## [0.0.8] - 2026-03-13

### Changed
- Fixed all clippy errors and warnings (83 total)
- Added crate-level `#![allow(...)]` for scaffolding code (PIN pad features not yet in use)
- Added pre-commit hook for `cargo fmt --check` and `cargo clippy -- -D warnings`
- Improved code quality: replaced manual range checks with `is_ascii_digit()`, used iterator patterns

## [0.0.7] - 2026-03-13

### Changed
- **Streamlined release artifacts**: Only `.bin` files are now released (following specter-diy pattern)
- Dropped `.elf` and `.hex` from releases - `.bin` is sufficient for all flashing tools
- Single `SHA256SUMS` file instead of individual `.sha256` files per artifact
- Release size reduced from 19 files to 4 files (3x `.bin` + 1x `SHA256SUMS`)

## [0.0.6] - 2026-03-13

### Added
- Multi-profile CI/CD: All 3 profiles now built and released
- Reproducibility improvements: Added `--remap-path-prefix` flag to build system for reproducible builds

### Changed
- Release workflow now publishes binaries for all 3 device profiles
- Artifact naming includes profile suffix for clear identification of profile-specific binaries

## [0.0.4] - 2026-03-13

### Fixed
- **Critical Boot Bug - SRAM Overflow**: Fixed memory.x configuration reducing SRAM from 320K to 256K to prevent HardFault on boot
- **Clock Configuration**: Changed HSI to HSE clock source for USB compatibility and stability
- Hardware-verified profiles (Cherry ST-2100 and Gemalto CT30) now working correctly

## [0.0.3] - 2026-03-12

### Changed
- Simplified to ARM-only build, removing x86_64 test support
- Converted lib.rs to `#![no_std]` for embedded environment
- Removed all `#[cfg(test)]` blocks from lib.rs and pinpad/mod.rs

### Fixed
- CI compatibility improvements
- Added thumbv7em-none-eabihf target to CI toolchain

## [0.0.2] - 2026-03-12

### Added
- USB CCID class implementation with full descriptor support
- Smartcard UART driver for ISO 7816 communication
- T=0 and T=1 protocol support with proper block handling
- PIN pad functionality with touchscreen integration via FT6x06
- **Device Profile Support** (3 profiles):
  - Cherry ST-2100 (VID:0x046A PID:0x003E) - Basic reader, no PIN pad
  - Gemalto Plain (VID:0x08E6 PID:0x3437) - Basic reader, no PIN pad
  - Gemalto PINpad (VID:0x08E6 PID:0x3438) - Full PIN pad support
- SecureState machine for PIN entry workflow
- Hardware touchscreen integration for PIN entry
- Mock PIN entry mode for testing without display
- APDU builder for PIN verification commands

### Fixed
- T=1 R-block N(R) computation now correctly derived from card's N(S) sequence number per ISO 7816-3 spec, fixing duplicate data in responses
- Gemalto Plain profile corrected to use Short APDU exchange level
- CI failure incident handling and documentation

## [0.0.1] - 2026-03-08

### Added
- Initial release
- STM32F469-DISCO firmware base
- USB infrastructure setup
- Smartcard interface initialization

### Fixed
- CI setup for cargo-binutils objcopy command installation

[Unreleased]: https://github.com/yourusername/ccid-reader/compare/v0.0.7...HEAD
[0.0.7]: https://github.com/yourusername/ccid-reader/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/yourusername/ccid-reader/compare/v0.0.4...v0.0.6
[0.0.4]: https://github.com/yourusername/ccid-reader/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/yourusername/ccid-reader/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/yourusername/ccid-reader/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/yourusername/ccid-reader/releases/tag/v0.0.1
