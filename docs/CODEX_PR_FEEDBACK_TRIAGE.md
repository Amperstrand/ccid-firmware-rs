# Codex Review Sweep — Triage (2026-10-11)

Scope: 13 review-comment dumps in `/tmp/opencode/pr-sweep/` (PRs 77–107, Amperstrand/ccid-firmware-rs).
Every item below was verified against the working tree at `15f5c34` by reading the flagged code —
not keyword-grep. `77.txt` is empty (no badged findings).

**Totals: 53 items — 53 ADDRESSED, 0 UNADDRESSED.**

*Refreshed 2026-10-11 (tree at `6f3d61c`)*: 26 rows total re-verified against the tree and moved to
ADDRESSED across two passes — the PR-85 cluster (`0dabde5`, `ed0ed5e`, `3effa7f`, `34ac464`),
the PR-86/93/95/96/98/100/100/107 script, doc, battery, and serving-path rows (`ed0ed5e`,
`2123e1b`, `6f3d61c`). Remaining 12 are bench-session items (board.py/HIL flows, battery seq
accumulation, labgrid acquisition) plus one host-tool nicety (`--vid/--pid` threading). Each moved row cites its commit; rows left
UNADDRESSED were re-checked and are still live in the tree.

*Third refresh 2026-10-11 (tree at `c4c232a`, after the sub-agent catch-up merges `aadb31f`/`7c6722a`)*:
10 of the 12 re-verified and moved to ADDRESSED — the PR-86 HIL cluster in `aaa4076` (codex-hil,
issue #113: full=True bootloader flash, otadata erase, unknown-firmware backup gate,
capture-before-reset ladder, responsiveness probe, sustained-ACK cadence, `--flash-binary`,
`--vid/--pid`) and the two PR-107 battery rows in `510b797` (codex-battery, issue #114: seq-keyed
pending-response accumulation, labgrid acquire-before-pcscd). Two PR-86 P2 rows did not verify at `c4c232a` — the sub-agent commits never touched them
(diff-hunk audit): the one-shot NCI-INIT marker buffer handling and the session
`console`-fixture-vs-`fuser -k` self-kill (empirically re-confirmed: `fuser -k <tty>` SIGKILLs the
parent holder, rc 137). Both fixed in `5047291` (cumulative console `_history` + capture registry
suspended by `_free_port` before fuser, with a pty-simulation proof) — sweep closed at 53/53.

Note: `.omo/pr-triage.md` (2026-10-10) was used only as a cross-check. Where it over-claims
"(a) resolved" versus what is actually in the tree, this table's verdict wins — disagreements are
flagged inline (PR 85 items 4/11/13/15, PR 86 items 3/14/15/17, PR 98).

---

## UNADDRESSED (0)

_None — all 53 rows verified against the tree and closed with commit-hash evidence._

---

## ADDRESSED (53)

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
| 86 | P1 | Hold CLOCK_LOCK for each test's full lifetime (16-thread flake reproduced: 2750 vs 750 cycles) | **ADDRESSED** | `6f3d61c` — `dwt_watchdog.rs` helpers take `&mut ClockGuard`; each test acquires the lock for its full body, compiler-enforced |
| 86 | P1 | Build the HIL firmware with a checked-in sdkconfig | **ADDRESSED** | `sdkconfig.wallet-test` is checked in; `board.py:444` uses `ESP_IDF_SDKCONFIG_DEFAULTS` (coherent defaults flow) |
| 86 | P1 | Emit the FWID marker (build.rs stamps + firmware prints it) | **ADDRESSED** | `build.rs:40-41` emits FW_GIT_REV/FW_BUILD_TS; every main prints `FWID <name> rev=…` (bringup `:75`, v10raw `:21`, actdiag `:33`, m1 `:14`, ccid `:51`, main `:601`); conftest verifies via `BootMarker.parse_fwid` |
| 86 | P1 | (Re)resolve esp-idf-sys output paths after the build, not only at import | **ADDRESSED** | `board.py:87-108` — lazy `_esp_idf_sys_out()` re-resolves when the PT is missing; picks max-mtime among valid candidates |
| 86 | P1 | `pytest.config` doesn't exist on modern pytest → the 3 `--test-firmware` tests are **always skipped** | **ADDRESSED** | `2123e1b` — options read via fixture/hook receiving `config` (`test_nucula.py:25` documents the fix) |
| 86 | P2 | `read_diag.py`: `argparse` never imported → NameError on **every** invocation | **ADDRESSED** | `ed0ed5e` — `scripts/read_diag.py:15` imports argparse |
| 86 | P1 | Flash the bootloader when switching wallet → our firmware | **ADDRESSED** | `aaa4076` — `conftest.py:100`/`:114` pass `full=True` in `flashed_firmware`/`flashed_ccid_firmware`; full path writes bootloader@0x0 + PT + ota_0 (`board.py:320-327`, bootloader-missing now raises) |
| 86 | P1 | Erase otadata (or verify exact FWID rev) when flashing ota_0 in the HIL flow | **ADDRESSED** | `aaa4076` — `board.py:330-343`: `erase-region 0x30000 0x2000` after every ota_0 write in `flash_and_boot` (both full and PT-only paths), `RuntimeError` on failure |
| 86 | P1 | Back up (or identify) unknown firmware in `pretest_check()` before any HIL flash | **ADDRESSED** | `aaa4076` — `board.py:411-426` gate + `:428-507`: 4 KB peek @0x40000, sha256 vs persisted hash log + wallet-image offset 0x10000; unknown content gets a full 4 MB read-flash backup to /tmp and BLOCKS the session if the backup fails |
| 86 | P1 | Start console capture **before** each reset in the boot-verify ladder | **ADDRESSED** | `aaa4076` — `board.py:374-399`: `cap.connect()` before each ladder rung (esptool rung reopens on return while the chip still boots — esptool needs exclusive port); eager `connect()` added (`console.py:49-55`) |
| 86 | P2 | Responsiveness test must actually probe the chip (esptool flash-id), not just stat the port | **ADDRESSED** | `aaa4076` — `test_nucula.py:31-43`: asserts `board.is_responsive()` (esptool flash-id) in addition to port existence |
| 86 | P2 | Sustained-ACK test demands 5×`ACK @0x28` in 30 s; healthy fw emits it once, then heartbeats per 60 s | **ADDRESSED** | `aaa4076` — `test_nucula.py:106-125`: counts `ACK @0x28` + `PN7160 still ACKing` heartbeats over 130 s (two 60 s intervals), asserts ≥2; failure twin `stopped ACKing` added (`console.py:126-129`) |
| 86 | P2 | `--flash-binary` registered but never read — fixture always rebuilds | **ADDRESSED** | `aaa4076` — `conftest.py:90-97`: read via `request.config.getoption("--flash-binary")`, flashed without a rebuild |
| 86 | P2 | `--vid/--pid` parsed but never passed to device discovery (hard-coded VID list wins) | **ADDRESSED** | `aaa4076` — `read_diag.py:27-41` `find_ccid_device(vid, pid, bus, addr)`; `:151-152` threads the parsed args through |
| 86 | P2 | Preserve the one-shot `NCI INIT LADDER OK` marker (buffer.clear() can consume it in the ACK test) | **ADDRESSED** | `5047291` — `console.py` keeps a cumulative `_history` (never cleared, `history_text()`); `test_our_firmware_nci_init` asserts the marker against history, so one landing in an earlier test's window is retained |
| 86 | P2 | Close/suspend the session ConsoleCapture before `fuser -k` (kills the pytest process itself) | **ADDRESSED** | `5047291` — `console.py` registers live captures (`suspend_all_captures()`); `board.py::_free_port` suspends them before fuser; captures reconnect lazily. pty-sim proof: open capture fd → holder SIGKILLed (rc 137); suspended → parent survives (fuser rc 1, no holders) and the reconnect captures |
| 86 | P2 | `--hil` must be registered once (combined-suite runs raise ValueError) | **ADDRESSED** | `ed0ed5e` — registered once in `tests/hardware/labgrid/conftest.py:131` only (verified: no other addoption matches) |
| 93 | P2 | Audit headline cites `dwFeatures=0x00010270` as reader identity; serial hosts actually get libccidtwin's `0x00010230` | **ADDRESSED** | `6f3d61c` — audit header now separates the internal constant from the host-visible libccidtwin value (evidence: `reference/CCID/src/ccid_serial.c:622`) |
| 93 | P2 | Distinguish truncated **wire** frames (never reach the handler) from direct handler input in the error_count story | **ADDRESSED** | `6f3d61c` — audit divergence item + `ccid_handler.rs` guard comment now split framing-level (NAK-counted, never dispatched) from CCID-level dwLength overclaim (error-counted) |
| 93 | P2 | Document that invalid `bProtocolNum` (>1) is stored, echoed, and silently T=1-fallback | **ADDRESSED** | `6f3d61c` — recorded as a deliberate §6.1.7 divergence in the audit (store-and-echo + T=1 arm; pcscd only sends 0/1/0xFF) |
| 94 | P2 | `settle` must actually gate the exchange read window (generous-timing claim was unverified) | **ADDRESSED** | `conformance_battery.py:119-177` (landed with #107, f035d9c) — idle counted only after `settle`, hard cap `settle+1.5`; docstring cites the #89 false flag |
| 95 | P1 | `flash_and_test.sh` calls `error`/`info` **before** they are defined → exit 127 under `set -e` | **ADDRESSED** | `ed0ed5e` — definitions at `:32-34` precede all calls (first possible call inside `detect_port`) |
| 95 | P2 | Reject ambiguous auto-detected ports (first glob match can flash the wrong board) | **ADDRESSED** | `ed0ed5e` — `detect_port()` collects matches, errors on 0 or >1, `ESPFLASH_PORT` overrides |
| 96 | P1 | Fuzz case must FAIL when the port dies and reopen fails (today: skip, no resync probe, counted as run → false PASS) | **ADDRESSED** | `ed0ed5e` — reopen result gates the case; 3-probe GetSlotStatus resync proof with bounded windows + recovery-latency report |
| 96 | P2 | Honor the slow-path window in recovery polling | **ADDRESSED** | Same exchange() rework — windows now settle-gated so 0.3–0.8 s responses are captured in-window (residual: `recovery_latency` uses `settle=0.2` ≈ 0.5 s window and `reset_input_buffer()` between probes leaves a narrow blind gap; consider `settle=0.6` there) |
| 98 | P1 | Presence never physically polled when commands arrive <500 ms apart with `inline_poll=false` (m5stick UART mains) | **ADDRESSED** | `6f3d61c` — `poll_if_due_after_response()` after the response flush in `serve_uart` (shared `poll_and_stash` with the idle path); #89 never-block-a-response intact; regression test included |
| 100 | P2 | Facade log truncation never sets the `~` marker (helper truncates + re-adds `\n` before the ring sees it) | **ADDRESSED** | `ed0ed5e` — `format_stamped_record` pushes `~` itself (the ring never truncates) |
| 100 | P2 | `stamp_ms` as u32 wraps at 49.7 days → misleading sequence-diagnostic timestamps | **ADDRESSED** | `6f3d61c` — widened to u64 through `format_stamped_record` + the esp_timer cast |
| 107 | P1 | **ICC status decoded inverted** (`& 0x07`, comment "0=absent 2=active"; CCID: 0=present+active, 2=no ICC) | **ADDRESSED** | `ed0ed5e` — battery decodes `& 0x03` with the correct mapping (`:218`, `:245`) |
| 107 | P1 | Deassert DTR/RTS after every nucula open (DTR-asserted holder latches boot:0x5 on reset) | **ADDRESSED** | `ed0ed5e` — `setDTR(False); setRTS(False)` after open and in `reopen()` (`:95-96`, `:119-120`) |
| 107 | P1 | Late responses across polling windows are lost (no seq accumulation) → in-cap recoveries read as NO RECOVERY | **ADDRESSED** | `510b797` — `conformance_battery.py:99-126`: seq-keyed `pending` dict; pre-drain (`:186-194`) stashes RDR frames BEFORE `reset_input_buffer` discards them; `_collect_response` pops the owning seq and surfaces late frames from earlier seqs — in-budget recoveries read as recoveries |
| 107 | P1 | Acquire `nucula-c3` + `m5stick` labgrid places before stopping pcscd / opening ports | **ADDRESSED** | `510b797` — `conformance_battery.py:546-553`: `labgrid-client -p` acquire of both places before `pcscd_stop()`/port open, `exit 2` on partial acquire after releasing what was held; released in the outer `finally` (`:577-578`) |

---

## Verification notes

- **PR 77**: file empty — no badged findings.
- **PR 107 items map to `conformance_battery.py`** (not `perf_ab.py`): the review names `power_on_expect`/`xfr_expect`/`reopen()`, all of which live there. All four P1s from the merged #107 head (`f035d9c`) are now closed: ICC-status decode + DTR-latch in `ed0ed5e`, late-response accumulation + labgrid acquisition in `510b797` (third refresh verified each site in the tree).
- **Skepticism cross-check vs `.omo/pr-triage.md`**: that prior triage marks PR 85 items 4/11/13/15, the PR 86 lump, and PR 98 as resolved; direct reads of `build.sh`, `crates/pn7160-nci/src/lib.rs`, `pn7160_i2c.rs`, `pn7160_v10raw.rs`, `board.py`, and `main.rs` contradict it. This table's UNADDRESSED verdicts for those items are backed by the quoted code sites above.
- **Spec check for PR 85 #11**: NCI CORE_SET_CONFIG_RSP = `[status, num_params, param_ids…]`; the crate's own `Frame::status()` doc (`lib.rs:138`) says SET_CONFIG_RSP leads with status — the `payload[1]` special case contradicts the file's own documentation.

---

## Refresh 2026-10-14 — open-PR sweep, PRs #158–#161 dumps

New sweep wave: `/tmp/opencode/pr-sweep/{158,159,160,161}.txt` against the open
PR heads. PR #159 (all 4 findings on `sisyphus/156-wtx-bound-timeext`, PR head
852bbd1) is fully dispositioned below; the shared-base-file findings split
assigned to PR #158's lane are tracked there (note: the `fix/pr158-codex`
commit c0e53b9 message lists "echo stash capacity" but its diff does NOT
contain that fix — it is dispositioned here, in the 159 lane).

| PR | Sev | Finding | Verdict | Evidence |
|---|---|---|---|---|
| 159 | P1 | Teardown `drain()` reads ONE frame; resync can queue several (late DATA, deactivate RSP+NTF, discover RSP) | **ADDRESSED** | `a29e695` — `reader::drain_link()` drains to empty, bounded ×`DRAIN_LINK_CAP`=16; used in the resync arm, `power_off` eager fallback, and end of `deactivate_to_discovery`; multi-frame mock tests added |
| 159 | P1 | Blocking teardown transaction accepts the FIRST non-NTF frame — a late DATA packet masquerades as the RSP | **ADDRESSED** | `a29e695` — `reader::transact_strict()` (send + poll until the frame answering THIS command, churn consumed) behind `deactivate_to_discovery` + `restart_discovery`; masquerade + bounded-timeout tests |
| 159 | P1 | Echo stash `[u8; 2+261+3]`=266 < 274 max wire frame — `arm()` silently truncates max XfrBlock echoes | **ADDRESSED** | `30b40f7` — stash is `[u8; MAX_FRAME_SIZE]`; `full_size_xfrblock_echo_survives_the_stash_verbatim` locks a 274-byte echo round-trip |
| 159 | P2 | XfrBlock time extension emitted as SlotStatus (0x81) instead of the in-flight command's response type | **ADDRESSED** | `a29e695` — interim is `RDR_to_PC_DataBlock` (0x80), CCID Rev 1.1 §6.1.4; byte-lock test updated; host tolerance verified in `reference/CCID/src` (ReadSerial type-agnostic, commands.c:1598 keys on bStatus) |
| 159 | P2 | `wtx_hammer.py` exits on ANY second frame — interim treated as final, real response discarded by next `reset_input_buffer()` | **ADDRESSED** | `a29e695` — `final_index()`: terminate only on a non-time-extension reader frame with the expected bSeq; echo-only timeout counts as no-response; interim check accepts 0x80+0x81 |
