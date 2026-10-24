<!-- GENERATED FILE — do not edit by hand (issue #183).
     Source of truth: scripts/gen_status.py fed by the hil-bench nightly
     workflow (bench inventory JSON + HIL JUnit XML). Every claim below
     carries its verification stamp. Regenerate; never hand-edit. -->

# Hardware & Firmware Status

> Auto-generated from the bench nightly run. `PASS` = verified by test;
> `SKIP` = hardware absent/busy (never a code failure); `FAIL` = broken.
> Human-readable history: the workflow run linked from the stamp.

## Bench wiring (labgrid)
| check | state | detail |
|---|---|---|
| coordinator reachable (46 resources total) | PASS |  |
| all 10 configured resource groups exported | PASS |  |
| place stm32-ccid: acquire/release | PASS |  |
| place nucula-c3: acquire/release | PASS | held by root@ai-legion (probe skipped) |
| place m5stick: acquire/release | PASS | held by root@ai-legion (probe skipped) |
| place ref-acr1252: acquire/release | PASS | held by root@ai-legion (probe skipped) |
| place ref-cardman: acquire/release | PASS |  |
| place stm32-f746: acquire/release | PASS |  |
| place ccid-nucula: acquire/release | FAIL | Server error: Error: Failed to acquire resources for place ccid-nucula |
| place ccid-m5stick: acquire/release | FAIL | Server error: Error: Failed to acquire resources for place ccid-m5stick |
| pcscd lists readers (12) | PASS |  |
| reader identity 'ST2XXX-001' present | FAIL | Virtual PICC boltcard vpcd 00 00; Virtual PICC boltcard vpcd 00 01; vPICC table card 00 00; vPICC table card 00 01; vJavaCard sim 00 00; vJavaCard sim 00 01; NR7101_SIM_bench 00 00; Nucula VPCD wifi67 00 00; Nucula VPCD wifi67 00 01; OMNIKEY AG CardMan 3121 00 00; ACS ACR1252 Dual Reader [ACR1252 Dual Reader PICC] 01 00; ACS ACR1252 Dual Reader [ACR1252 Dual Reader SAM] 02 00 |
| reader identity 'ACR1252' present | PASS |  |
| reader identity 'CardMan' present | PASS |  |
| exactly one DUT reader (serial ST2XXX-001) | FAIL |  |
| reference readers carry no DUT serial (3 found) | PASS |  |
| F469/F746 DUT (Cherry emul) on USB | FAIL |  |
| ST-LINK V2.1 on USB | FAIL |  |
| ACR1252 reference on USB | PASS |  |
| CardMan reference on USB | PASS |  |
| disk under /tmp/opencode below 85% | PASS |  |

Held (healthy-but-busy, probes skipped): `android-emulator` (root@ai-legion), `gs1900-8hp-1` (root@ai-legion), `m5stick` (root@ai-legion), `nucula-c3` (root@ai-legion), `phone-s20` (root@ai-legion), `ref-acr1252` (root@ai-legion), `ws3915i-ad97` (ubuntu@ai-legion-small)

Verified: 2026-10-24T03:32:01Z @ `2616c50c458e` — `bench_inventory.py`

## HIL suites (on-device)
| suite | pass | skip | fail |
|---|---|---|---|
| tests.hardware.labgrid.test_acr1252_emu_hil | 0 | 3 | 0 |
| tests.hardware.labgrid.test_bench_coordination.BenchLockModeTests | 2 | 0 | 0 |
| tests.hardware.labgrid.test_bench_coordination.SelectedDutsTests | 3 | 0 | 0 |
| tests.hardware.labgrid.test_bench_coordination.YieldAcquiredPlaceTests | 3 | 0 | 0 |
| tests.hardware.labgrid.test_bench_state | 1 | 5 | 0 |
| tests.hardware.labgrid.test_ccid_hil | 0 | 7 | 0 |
| tests.hardware.labgrid.test_esp32_flash.BaudDisciplineTests | 2 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.BootCheckTests | 8 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.CliBootCheckResolutionTests | 4 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.ConsoleNoneDefaultTests | 4 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.EntryPointTests | 6 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.HygieneTests | 6 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.JtagFallbackCaptureTests | 7 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.JtagResetDecisionTests | 5 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.LadderTests | 2 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.MarkerUntilRestartTests | 3 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.ObservabilityTests | 6 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.OrderingTests | 2 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.QuiesceUnitTests | 3 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.SerialParseTests | 3 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.SysfsTests | 2 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.UnitRestartRaiseTests | 1 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.VerdictCarriesRestoreTest | 1 | 0 | 0 |
| tests.hardware.labgrid.test_esp32_flash.WireBootCheckTests | 3 | 0 | 0 |
| tests.hardware.labgrid.test_gempc_smoke.DiagnosticsTests | 5 | 0 | 0 |
| tests.hardware.labgrid.test_gempc_smoke.FramingTests | 5 | 0 | 0 |
| tests.hardware.labgrid.test_serial_battery_hil | 0 | 2 | 0 |
| tests.hardware.labgrid.test_stm32_flash.FlashArgTests | 9 | 0 | 0 |
| tests.hardware.labgrid.test_stm32_flash.ParserTests | 3 | 0 | 0 |

Verified: 2026-10-24T03:32:01Z @ `2616c50c458e` — nightly HIL selection per `.github/workflows/hil-bench.yml` (SKIP = place busy/hardware absent, per doctrine)

## Board-level notes

Per-board deep state (BLE, GP cards, known gaps) lives in the tracking issues it belongs to (#66 BLE, #127 GP cards, #183 this pipeline). This file only carries what a run just verified.

<!-- GENERATED by scripts/status_drift.py (#183) — drift layer, never edit. -->
## Bench-evidence drift (since the last STATUS stamp)

Last bench verified main @ `c2ac4a63a9ac`; this report @ `2616c50c458e` (15 commits between). A target listed below has firmware landed AFTER its last on-hardware verification — its STATUS rows are stale until the next bench run.

| target | commits awaiting bench | oldest | flag |
|---|---|---|---|
| bench-infra | 4 | tools(hil): enforce design §4.4 checks 4-7 in the bundle producer | ⚠️ STALE |
| esp32-shared | 4 | build(#308): FW_BUILD_TS honors SOURCE_DATE_EPOCH (reproducible-pinnable images) (#440) | ⚠️ STALE |
| stm32-f469-f746 | 3 | perf(#408): short first wait in the NCI collect loops (#445) | ⚠️ STALE |

<details><summary>bench-infra: 4 commits</summary>

- `653c626242d3` tools(hil): enforce design §4.4 checks 4-7 in the bundle producer
- `96ed4d323bc4` tools(hil): flash_bundle.py — §4.4 self-checks + §4.5 refusal matrix + CLI
- `75dce5025a42` docs(hil): FLASH-BUNDLES README (usage + refusal matrix) + CHANGELOG
- `81eb35f6b57a` fix(battery): single-reader runs, name-drift-proof restart probe, card-aware GetSlotStatus

</details>

<details><summary>esp32-shared: 4 commits</summary>

- `9d9fc7cfad77` build(#308): FW_BUILD_TS honors SOURCE_DATE_EPOCH (reproducible-pinnable images) (#440)
- `a0b1f7420fc8` perf(#408): short first wait in the NCI collect loops (#445)
- `2797ea9a5527` deps: bump pn532-transport pin to bolty main cb3ad97 (drift row; additive F469 port + conformance vectors since 2a1afea)
- `3f32090b4ba5` style: cargo fmt -p pn7160-nci — main shipped unformatted via #445 (stm32-lint red on every ci-run PR since)

</details>

<details><summary>stm32-f469-f746: 3 commits</summary>

- `a0b1f7420fc8` perf(#408): short first wait in the NCI collect loops (#445)
- `2797ea9a5527` deps: bump pn532-transport pin to bolty main cb3ad97 (drift row; additive F469 port + conformance vectors since 2a1afea)
- `3f32090b4ba5` style: cargo fmt -p pn7160-nci — main shipped unformatted via #445 (stm32-lint red on every ci-run PR since)

</details>

