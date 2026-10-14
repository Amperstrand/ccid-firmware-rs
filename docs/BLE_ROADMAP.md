# BLE Console Roadmap — nucula (C3) + m5stick (xtensa)

State snapshot: 2026-10-14. Tracking issue: #66.

## What works today

| Board | Path | State |
|---|---|---|
| m5stick (`m5stick-ble` board) | BLE NUS GATT log console, UART0 = CCID | **Worked** (2026-10-09 bench, parked). Re-verification on current main (IDF 5.5.5, fork v1.98.1.0) pending — blocked by cmake cache contamination, see below. |
| m5stick (`m5stick-spp` board) | pcscd over BT-Classic SPP, RFCOMM = CCID wire | **Bench-verified** 2026-10-11 (ATR + APDU, 30/30 soak, p50 48 ms/APDU). |
| nucula (`c3-ccid-ble` board, new) | CCID on USB-CDC + BLE NUS console | **Compiles, boots, GATT chain completes — advertising rejected 0x0c** (bench-reproduced 2026-10-14, hash-verified flash). |

## The C3 blocker (issue #66) — what we now KNOW

1. NOT a sdkconfig problem: 42-on/50-off verified present in the effective build
   (`sdkconfig-c3-ble.full`; the `ble` cargo feature fails compilation against
   BT-off configs, and this build compiled + ran with BT up).
2. NOT a stale-cmake-cache artifact: the 2026-10-14 repro went through the
   `build.sh c3-ccid-ble` cache-stamped flow.
3. NOT the event chain: canonical ordering (adv-data complete → scan-rsp
   write → scan-rsp complete → `start_advertising`) — identical chain
   advertises fine on the xtensa m5stick.
4. The controller answers LE Set Advertising Parameters with Command
   Disallowed (0x0c) at the `start_advertising` call; Bluedroid surfaces it
   as `AdvertisingStarted(status != Success)`; our `check_bt_status`
   collapses it to `ESP_FAIL`.

## Ranked next steps

1. **Surface the real status**: log the actual `BtStatus` / ESP_GAP event
   status in `ble_debug.rs` instead of collapsing to ESP_FAIL; build a
   diagnostic variant with BT_BTM layer logs unsilenced (`silence_c_logs`
   bypass) to capture the exact HCI reject + any preceding HCI commands.
2. **Diff the working xtensa path**: same Rust chain, same sdkconfig family —
   bisect the difference (controller ROM behavior vs BTA config). Check
   whether the C3 requires an explicit `esp_ble_gap_config_local_privacy` /
   address setup before first advertising (a classic 0x0c source on
   RISC-V controllers is the controller seeing advertising already armed by
   an earlier aborted attempt — power-cycle vs soft-reset differences).
3. **NimBLE fallback**: if Bluedroid-on-C3 stays opaque, move the C3 console
   to the NimBLE host (IDF ships both; NUS is a simple service). NimBLE is
   the better-maintained host on C3 silicon.
4. **m5stick-ble re-verification**: clear the xtensa cache contamination
   (`rm -rf /root/.cargo-target/xtensa-esp32-espidf/*/build/esp-idf-sys-*`,
   caused by the stale `/tmp/opencode/ccid-idf555` worktree), rebuild
   `./build.sh m5stick-ble`, flash, confirm advertising with
   `tests/hardware/ble/ble_log_capture.py --list`, then restore the standard
   m5stick CCID image on the bench.

## Related

- PR #158/#159 Codex feedback fix briefs: `/tmp/opencode/pr-sweep/brief-pr15{8,9}.md`
  (worktrees `/tmp/ccid-pr158`, `/tmp/ccid-pr159` prepared).
- Bench note 2026-10-14: the nucula now runs the `c3-ccid-ble` debug build
  (still a full pn7160-ccid firmware + BLE console); re-flash the standard
  `c3-ccid` image when the BLE work pauses.
