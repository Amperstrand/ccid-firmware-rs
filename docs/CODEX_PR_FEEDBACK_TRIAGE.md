# Codex Review Sweep — Triage (2026-10-11)

Scope: 13 review-comment dumps in `/tmp/opencode/pr-sweep/` (PRs 77–107, Amperstrand/ccid-firmware-rs).
Every item below was verified against the working tree at `15f5c34` by reading the flagged code —
not keyword-grep. `77.txt` is empty (no badged findings).

**Totals: 53 items — 29 ADDRESSED, 24 UNADDRESSED.**

*Refreshed 2026-10-11 (tree at `34ac464`)*: 14 rows re-verified against the tree and moved to
ADDRESSED — the PR-85 cluster landed in `0dabde5`, `ed0ed5e`, `3effa7f`, `34ac464`, and the
PR-86 script/pytest rows in `ed0ed5e`/`2123e1b`. Each moved row cites its commit; rows left
UNADDRESSED were re-checked and are still live in the tree.

Note: `.omo/pr-triage.md` (2026-10-10) was used only as a cross-check. Where it over-claims
"(a) resolved" versus what is actually in the tree, this table's verdict wins — disagreements are
flagged inline (PR 85 items 4/11/13/15, PR 86 items 3/14/15/17, PR 98).

---

## UNADDRESSED (24)

| PR | Priority | Feedback summary | Status | Suggested action |
|----|----------|------------------|--------|------------------|
| 86 | P1 | Flash the bootloader when switching wallet → our firmware | **UNADDRESSED** | `conftest.py:96` calls `flash_and_boot(binary)` with `full=False` → writes PT+app only after the wallet flashed its own bootloader (`board.py:314-315`). The `full=True` path exists (`board.py:310-313`) — pass it in the fixture (or always after `flashed_wallet`) |
| 86 | P2 | Close/suspend the session ConsoleCapture before `fuser -k` (kills the pytest process itself) | **UNADDRESSED** | Session-scoped `console` fixture (`conftest.py:72-77`) is never closed before `_free_port()`'s `fuser -k` (`board.py:114-129`) during the firmware-flash fixtures |
| 86 | P2 | Responsiveness test must actually probe the chip (esptool flash-id), not just stat the port | **UNADDRESSED** | `test_nucula.py:22-28` asserts `os.path.exists` only; `board.is_responsive()` exists (`board.py:433-437`) but is unused here |
| 86 | P2 | Preserve the one-shot `NCI INIT LADDER OK` marker (buffer.clear() can consume it in the ACK test) | **UNADDRESSED** | `test_nucula.py:64,81` still clear the shared console buffer per test; firmware emits the marker once (`pn7160_bringup.rs:242`). Keep cumulative history or assert from one capture |
| 86 | P2 | Sustained-ACK test demands 5×`ACK @0x28` in 30 s; healthy fw emits it once, then heartbeats per 60 s | **UNADDRESSED** | `test_nucula.py:97-102` unchanged; fw cadence (`pn7160_bringup.rs:268-273`) unchanged. Count `PN7160 still ACKing` or widen the window |
| 86 | P1 | Hold CLOCK_LOCK for each test's full lifetime (16-thread flake reproduced: 2750 vs 750 cycles) | **UNADDRESSED** | `firmware/ccid-firmware/src/dwt_watchdog.rs:206-214` — guards drop when the helpers return; inter-op windows remain. Return the guard or wrap tests in a serializer |
| 86 | P2 | `--vid/--pid` parsed but never passed to device discovery (hard-coded VID list wins) | **UNADDRESSED** | `scripts/read_diag.py:145` calls `find_ccid_device(args.bus, args.address)`; `:26-35` ignores vid/pid. Thread them through |
| 86 | P2 | `--flash-binary` registered but never read — fixture always rebuilds | **UNADDRESSED** | `tests/hardware/nucula/conftest.py:36-37` registers it; no reader anywhere in `tests/hardware/nucula/`. Resolve from `request.config` in `flashed_firmware` |
| 86 | P1 | Erase otadata (or verify exact FWID rev) when flashing ota_0 in the HIL flow | **UNADDRESSED** | `board.py` flash paths (`:173-185`, `:309-315`) write PT+ota_0 only — no `0x30000` erase — and `expect=["FWID"]` (`:317`) accepts any revision, so a stale ota_1 image can pass. (build.sh does erase otadata — different flow.) ⚠️ prior triage claims "otadata erase in flash ladder" — not in board.py |
| 86 | P1 | Back up (or identify) unknown firmware in `pretest_check()` before any HIL flash | **UNADDRESSED** | `board.py:364-374` checks port/holders/esptool only. AGENTS.md documents the backup *rule* (failure mode 5) but nothing implements it. Capture full flash or refuse on unknown image |
| 86 | P1 | Start console capture **before** each reset in the boot-verify ladder | **UNADDRESSED** | `board.py:346-352` opens ConsoleCapture after each reset step returns. Mitigated (bringup re-issues FWID in the 5 s probe loop, `pn7160_bringup.rs:156`), but the ordering flaw remains — concurrent capture or exact-rev assertion still needed |
| 93 | P2 | Audit headline cites `dwFeatures=0x00010270` as reader identity; serial hosts actually get libccidtwin's `0x00010230` | **UNADDRESSED** | `docs/ESP32_CCID_SPEC_AUDIT.md:22-23` still leads with 0x00010270; `:215` footnote acknowledges but doesn't correct. Label the constant as unused-internal + state the host-visible value |
| 93 | P2 | Distinguish truncated **wire** frames (never reach the handler) from direct handler input in the error_count story | **UNADDRESSED** | `ccid_handler.rs:88-107` guard still commented "Truncated transport frame"; the serial server resets the parser on stall without calling `process_command` (`ccid_serial_server.rs:153-173`), so wire truncations are never counted. One corrective sentence in code comment + audit |
| 93 | P2 | Document that invalid `bProtocolNum` (>1) is stored, echoed, and silently T=1-fallback | **UNADDRESSED** | `ccid_handler.rs:405` stores `header.specific[0]` unconditionally; response echoes it; audit (`ESP32_CCID_SPEC_AUDIT.md:204-206`) records payload non-validation but not the protocol-number gap. Validate per CCID §6.1.7 or record the divergence |
| 95 | P1 | `flash_and_test.sh` calls `error`/`info` **before** they are defined → exit 127 under `set -e` | **UNADDRESSED** | Calls at `flash_and_test.sh:40-44`; definitions at `:46-53`. Move the block up. Checked-in host tool is broken today |
| 95 | P2 | Reject ambiguous auto-detected ports (first glob match can flash the wrong board) | **UNADDRESSED** | `detect_port()` `flash_and_test.sh:30-37` returns the first match. Collect matches, require exactly one unless `ESPFLASH_PORT` |
| 96 | P1 | Fuzz case must FAIL when the port dies and reopen fails (today: skip, no resync probe, counted as run → false PASS) | **UNADDRESSED** | `conformance_battery.py:330-332` — `r.reopen()` result ignored, `continue` skips the GetSlotStatus resync proof, case still counted in `len(cases)` (`:359`) |
| 98 | P1 | Presence never physically polled when commands arrive <500 ms apart with `inline_poll=false` (m5stick UART mains) | **UNADDRESSED** | `main.rs:213-253` `serve_uart`: polls only in `on_read_idle` (`:250`); no poll after flushing a response; `inline_poll: false` (`:203`) means `feed_byte` never polls (codified by `no_inline_poll_profile_never_polls_on_get_slot_status`). Poll-due after response flush fixes it. (USB-CDC main is covered via `inline_poll=true`, `pn7160_ccid.rs:148/170`.) ⚠️ prior triage marks ADDRESSED — that reading misses the inline_poll=false profile the review targeted |
| 100 | P2 | Facade log truncation never sets the `~` marker (helper truncates + re-adds `\n` before the ring sees it) | **UNADDRESSED** | `log_shim.rs:221-229` truncates to 199 + `\n`; ring (`:519 push_line`) sees a complete line. Append `~` in the helper or let the ring truncate |
| 100 | P2 | `stamp_ms` as u32 wraps at 49.7 days → misleading sequence-diagnostic timestamps | **UNADDRESSED** | `log_shim.rs:511` `(... / 1000) as u32`. Keep u64 or note the epoch |
| 107 | P1 | **ICC status decoded inverted** (`& 0x07`, comment "0=absent 2=active"; CCID: 0=present+active, 2=no ICC) | **UNADDRESSED** | Merged as-is (battery last touched at #107 head f035d9c). `conformance_battery.py:211` `resp[0][2] & 0x07`; active card fails the GSS case (`:238`) and absent card bypasses `power_on_expect`/`xfr_expect` (`:216-230`). Decode `& 0x03` via ccid-core's `ICC_STATUS_*` |
| 107 | P1 | Late responses across polling windows are lost (no seq accumulation) → in-cap recoveries read as NO RECOVERY | **UNADDRESSED** | `conformance_battery.py:145-177` — per-call buffer + `reset_input_buffer()` (`:150`); no pending-sequence carry. Keep one seq pending to its deadline or accumulate |
| 107 | P1 | Deassert DTR/RTS after every nucula open (DTR-asserted holder latches boot:0x5 on reset) | **UNADDRESSED** | `Reader.__init__` `conformance_battery.py:91` and `reopen()` `:113` use plain `serial.Serial(...)`; AGENTS.md confirms this exact hardening is tracked-open (2026-10-10 latch recurrence). `setDTR(False); setRTS(False)` after open |
| 107 | P1 | Acquire `nucula-c3` + `m5stick` labgrid places before stopping pcscd / opening ports | **UNADDRESSED** | `conformance_battery.py` has no labgrid usage; stops pcscd directly (`:180-183`). Acquire (or share the pytest `bench_lock`/place machinery from the #99 framework), release in teardown |

---

## ADDRESSED (29)

| PR | Priority | Feedback summary | Status | Suggested action |
|----|----------|------------------|--------|------------------|
| 79 | P1 | Flash Xtensa apps into the OTA slot (0x40000), not otadata@0x30000 | **ADDRESSED** | `build.sh:140,151,162` — `FLASH_OFFSET="0x40000"` with "(Codex #79 P1)" comments on all three M5 boards |
| 79 | P2 | Create TARGET_DIR before writing the sdkconfig stamp | **ADDRESSED** | `build.sh:283` — `mkdir -p "${TARGET_DIR}"` immediately before the stamp `cp` |
| 79 | P2 | Missing stamp + existing caches must invalidate (unknown provenance) | **ADDRESSED** | `build.sh:219-226` — no-stamp → scan esp-idf-sys CMakeCaches → STALE=1 → drop before build |
| 81 | P2 | Reader-local replies (GET UID / pseudo-APDU SW) must not count apdu_rx | **ADDRESSED** | `ccid_handler.rs:370-371,388` — "no card I/O, so no rx counting (Codex review on #81)"; rx counted only after successful transmit (`:470`) |
| 81 | P2 | Refresh uptime at dispatch (not only card-poll-gated) before 0xD0 answers | **ADDRESSED** | `ccid_serial_server.rs:202-204` dispatch-time `refresh_diagnostics` citing "Codex review on #81"; regression test `uptime_refreshes_under_continuous_non_poll_traffic` (`:680`) |
| 82 | P1 | Erase otadata when flashing ota_0 so the bootloader stops booting stale ota_1 | **ADDRESSED** | `build.sh:302-306` — `erase-region 0x30000 0x2000` after write-flash, comment cites "Codex review on #82" |
| 85 | P1 | Define SCRIPT_DIR before expanding it (set -u crash) | **ADDRESSED** | `build.sh:21` assigns SCRIPT_DIR before any expansion (workspace pin at `:29`); `--help` path clean |
| 85 | P1 | Use `CONFIG_MAIN_TASK_STACK_SIZE` (not the nonexistent ESP_MAIN variant) in sdkconfig.wallet-test | **ADDRESSED** | `sdkconfig.wallet-test:6` — `CONFIG_MAIN_TASK_STACK_SIZE=16384`; file is git-tracked |
| 85 | P2 | Configure bit-bang pins INPUT_OUTPUT_OD so ACK/idle sampling reads the line | **ADDRESSED** | `pn7160_bitbang.rs:23-25` — `GPIO_MODE_INPUT_OUTPUT_OD` with explanatory comment |
| 85 | P1 | Stamp/clean esp-idf-sys cache when the **IDF version pin** changes (stamp is sdkconfig-only) | **ADDRESSED** | `3effa7f` — `build.sh` stamp body = `# esp_idf_version:` header (parsed from Cargo.toml metadata) + sdkconfig; version bump with unchanged sdkconfig now drops the cmake caches |
| 85 | P2 | **CORE_SET_CONFIG_RSP status is byte 0**; code read byte 1 and mock pushed `[0x01, 0x00]` (= REJECTED) | **ADDRESSED** | `ed0ed5e` — `crates/pn7160-nci/src/lib.rs:141` reads `payload[0]`; mocks corrected |
| 85 | P2 | Bit-bang probe must poll `read_scl()` with timeout (clock stretching) before sampling ACK | **ADDRESSED** | `34ac464` — `pn7160_bitbang.rs` `scl_high_bounded()` at every SCL rise (UM10204 §3.1.9), bounded ~320 µs, stuck line logs + reads as NAK |
| 85 | P2 | Install a logger before `pn7160-bitbang` diagnostics (all output discarded today) | **ADDRESSED** | `3effa7f` — `EspLogger::initialize_default()` before the first `log::warn!` |
| 85 | P2 | Cycle VEN before the bit-bang address probe (chip is in power-down on cold boot → NAK is meaningless) | **ADDRESSED** | `34ac464` — `ven_wake()` runs the raw v10raw sequence (high 10 ms, low 50 ms, high 50 ms) before any probing; module outcome table updated |
| 85 | P2 | build.rs should watch the **resolved git ref**, not `.git/HEAD` (commit advance leaves HEAD unchanged → stale FW_GIT_REV) | **ADDRESSED** | `3effa7f` — `rev-parse --git-dir` (worktree-aware) + symref resolution + packed-refs, existence-guarded; bench-proven: a commit marks the package Dirty on the ref file |
| 85 | P2 | v10raw delays are raw ticks (`vTaskDelay(10)/(50)` = 100/500 ms at HZ=100, not 10/50 ms) | **ADDRESSED** | `3effa7f` — all sites route through `delay_ticks(ms)` = `ms * configTICK_RATE_HZ / 1000` (pdMS_TO_TICKS expansion) |
| 85 | P2 | IRQ-wait budget counts µs while `delay_ms(1)` sleeps one 10 ms tick (HZ=100) → 200 ms budget ≈ 2 s | **ADDRESSED** | `3effa7f` — `pn7160_i2c.rs` counts `RTOS_TICK_US` (10 000) per iteration |
| 85 | P2 | ven-never constructor never drives VEN high (chip stays in power-down; experiment always fails) | **ADDRESSED** | `0dabde5` — `gpio_set_level(VEN_PIN, 1)` in the ven-never constructor with the Codex-PR-85 comment |
| 85 | P2 | verdict-C recovery re-runs baseline `ven_cycle()` instead of the verdict-aware `ven_retrigger` | **ADDRESSED** | `0dabde5` — `pn7160_bringup.rs:196` health-probe NAK path calls `ven_retrigger(t)` |
| 85 | P2 | v10raw no-op ISR can storm on warm reset (IRQ already high when handler added); production ISR quenches | **ADDRESSED** | `3effa7f` — `dummy_isr` calls `gpio_intr_disable(IRQ)`, same contract as `pn7160_i2c.rs::dummy_irq_isr` |
| 85 | P2 | Install a logger before pn7160-bringup diagnostics | **ADDRESSED** | `pn7160_bringup.rs:73` — `crate::netlog::init()` restored before the FWID line; `netlog.rs:45-47` installs a real boxed logger |
| 86 | P1 | Build the HIL firmware with a checked-in sdkconfig | **ADDRESSED** | `sdkconfig.wallet-test` is checked in; `board.py:444` uses `ESP_IDF_SDKCONFIG_DEFAULTS` (coherent defaults flow) |
| 86 | P1 | Emit the FWID marker (build.rs stamps + firmware prints it) | **ADDRESSED** | `build.rs:40-41` emits FW_GIT_REV/FW_BUILD_TS; every main prints `FWID <name> rev=…` (bringup `:75`, v10raw `:21`, actdiag `:33`, m1 `:14`, ccid `:51`, main `:601`); conftest verifies via `BootMarker.parse_fwid` |
| 86 | P1 | (Re)resolve esp-idf-sys output paths after the build, not only at import | **ADDRESSED** | `board.py:87-108` — lazy `_esp_idf_sys_out()` re-resolves when the PT is missing; picks max-mtime among valid candidates |
| 86 | P1 | `pytest.config` doesn't exist on modern pytest → the 3 `--test-firmware` tests are **always skipped** | **ADDRESSED** | `2123e1b` — options read via fixture/hook receiving `config` (`test_nucula.py:25` documents the fix) |
| 86 | P2 | `read_diag.py`: `argparse` never imported → NameError on **every** invocation | **ADDRESSED** | `ed0ed5e` — `scripts/read_diag.py:15` imports argparse |
| 86 | P2 | `--hil` must be registered once (combined-suite runs raise ValueError) | **ADDRESSED** | `ed0ed5e` — registered once in `tests/hardware/labgrid/conftest.py:131` only (verified: no other addoption matches) |
| 94 | P2 | `settle` must actually gate the exchange read window (generous-timing claim was unverified) | **ADDRESSED** | `conformance_battery.py:119-177` (landed with #107, f035d9c) — idle counted only after `settle`, hard cap `settle+1.5`; docstring cites the #89 false flag |
| 96 | P2 | Honor the slow-path window in recovery polling | **ADDRESSED** | Same exchange() rework — windows now settle-gated so 0.3–0.8 s responses are captured in-window (residual: `recovery_latency` uses `settle=0.2` ≈ 0.5 s window and `reset_input_buffer()` between probes leaves a narrow blind gap; consider `settle=0.6` there) |

---

## Verification notes

- **PR 77**: file empty — no badged findings.
- **PR 107 items map to `conformance_battery.py`** (not `perf_ab.py`): the review names `power_on_expect`/`xfr_expect`/`reopen()`, all of which live there. The battery's current form **is** the merged #107 head (`f035d9c`, 2026-10-09) — all four P1s were merged unfixed and remain live.
- **Skepticism cross-check vs `.omo/pr-triage.md`**: that prior triage marks PR 85 items 4/11/13/15, the PR 86 lump, and PR 98 as resolved; direct reads of `build.sh`, `crates/pn7160-nci/src/lib.rs`, `pn7160_i2c.rs`, `pn7160_v10raw.rs`, `board.py`, and `main.rs` contradict it. This table's UNADDRESSED verdicts for those items are backed by the quoted code sites above.
- **Spec check for PR 85 #11**: NCI CORE_SET_CONFIG_RSP = `[status, num_params, param_ids…]`; the crate's own `Frame::status()` doc (`lib.rs:138`) says SET_CONFIG_RSP leads with status — the `payload[1]` special case contradicts the file's own documentation.
