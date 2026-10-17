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
| coordinator reachable (52 resources total) | PASS |  |
| all 10 configured resource groups exported | PASS |  |
| place stm32-ccid: acquire/release | PASS |  |
| place nucula-c3: acquire/release | PASS |  |
| place m5stick: acquire/release | PASS | held by root@ai-legion (probe skipped) |
| place ref-acr1252: acquire/release | PASS |  |
| place ref-cardman: acquire/release | PASS |  |
| place stm32-f746: acquire/release | PASS |  |
| place ccid-nucula: acquire/release | PASS |  |
| place ccid-m5stick: acquire/release | FAIL | Server error: Error: Failed to acquire resources for place ccid-m5stick |
| pcscd lists readers (3) | PASS |  |
| reader identity 'ST2XXX-001' present | PASS |  |
| reader identity 'ACR1252' present | FAIL | NR7101_SIM_bench 00 00; OMNIKEY AG CardMan 3121 00 00; Cherry GmbH SmartTerminal ST-2xxx (ST2XXX-001) 01 00 |
| reader identity 'CardMan' present | PASS |  |
| exactly one DUT reader (serial ST2XXX-001) | PASS |  |
| reference readers carry no DUT serial (1 found) | FAIL | OMNIKEY AG CardMan 3121 00 00 |
| F469/F746 DUT (Cherry emul) on USB | PASS |  |
| ST-LINK V2.1 on USB | PASS |  |
| ACR1252 reference on USB | PASS |  |
| CardMan reference on USB | PASS |  |
| disk under /tmp/opencode below 85% | FAIL | 93% used (872G/936G) |

Held (healthy-but-busy, probes skipped): `android-emulator` (root@ai-legion), `gs1900-8hp-1` (ubuntu@ai-legion-small), `m5stick` (root@ai-legion), `vlab-default` (root@ai-legion)

Verified: 2026-10-17T03:31:18Z @ `0f45f31743e9` — `bench_inventory.py`

## HIL suites (on-device)
| suite | pass | skip | fail |
|---|---|---|---|
| tests.hardware.labgrid.test_acr1252_emu_hil | 0 | 3 | 0 |
| tests.hardware.labgrid.test_bench_state | 2 | 3 | 1 |
| tests.hardware.labgrid.test_ccid_hil | 7 | 0 | 0 |
| tests.hardware.labgrid.test_serial_battery_hil | 0 | 2 | 0 |
| tests.hardware.labgrid.test_stm32_flash.FlashArgTests | 5 | 0 | 0 |
| tests.hardware.labgrid.test_stm32_flash.ParserTests | 3 | 0 | 0 |

Verified: 2026-10-17T03:31:18Z @ `0f45f31743e9` — nightly HIL selection per `.github/workflows/hil-bench.yml` (SKIP = place busy/hardware absent, per doctrine)

## Board-level notes

Per-board deep state (BLE, GP cards, known gaps) lives in the tracking issues it belongs to (#66 BLE, #127 GP cards, #183 this pipeline). This file only carries what a run just verified.
