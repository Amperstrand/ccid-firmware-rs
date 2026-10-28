# OTA update + rollback hardware suite

Pushes firmware images to the wifi-vpcd boards over the OTA wire protocol
(TCP :3232) and verifies the board's behavior: the positive path (good
image, board comes back healthy) and the negative paths (crafted bad
images must never take the bench down; the bootloader must roll back to
the previous good image when rollback is the defense engaged).

Nothing here runs by default: the e2e surface is gated on
`NUCULA_OTA_E2E=1` (same opt-in convention as `tests/hardware/vpcd`).
The local unit tests (`ota_unit` marker) run anywhere - no board, no
pyscard. This suite was authored while the bench was busy; it has been
self-checked with `pytest --collect-only`, the local unit tests and
`py_compile`, but NOT executed against hardware yet.

## Research: ESP-IDF OTA rollback semantics

Primary sources (consulted 2026-10-13):

- ESP-IDF "Over The Air Updates (OTA)" - App Rollback / Anti-Rollback /
  otatool sections:
  https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/ota.html
  (pinned bench version of the same page:
  .../en/v5.5.5/esp32c3/api-reference/system/ota.html - identical text
  to the vendored copy at `.embuild/espressif/esp-idf/v5.5.5/docs/en/
  api-reference/system/ota.rst`)
- esp-idf OTA examples: `examples/system/ota` (vendored under
  `.embuild/espressif/esp-idf/v5.5.5/examples/system/ota/`; upstream
  https://github.com/espressif/esp-idf/tree/master/examples/system/ota)
- API + implementation, vendored v5.5.5:
  `components/app_update/include/esp_ota_ops.h`,
  `components/app_update/esp_ota_ops.c` (`esp_ota_end` ->
  `ota_verify_partition` -> `esp_image_verify`),
  `components/bootloader_support/src/bootloader_utility.c`
  (boot-slot selection + fallback)

The semantics this suite encodes:

1. **`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`** (bootloader Kconfig,
   baked into the compiled bootloader, not the app) enables the
   pending-verify state machine. When set, `esp_ota_set_boot_partition()`
   writes the new slot's state as `ESP_OTA_IMG_NEW` into the **otadata**
   partition.
2. **Pending verify**: on the next boot the bootloader demotes `NEW` to
   `ESP_OTA_IMG_PENDING_VERIFY`. The application must confirm itself by
   calling **`esp_ota_mark_app_valid_cancel_rollback()`** (state becomes
   `ESP_OTA_IMG_VALID`). Per the docs, *"a new application has only one
   attempt to boot successfully"* - there is no boot-count or wall-clock
   timeout in mainline IDF; the recommendation to self-test quickly
   exists because any RESET before confirmation is treated as failure.
3. **Rollback**: if a reset occurs while the slot is still
   `PENDING_VERIFY` (app crashed, bootlooped, watchdogged, lost power,
   or idled until power-cycled), the bootloader marks the slot
   `ESP_OTA_IMG_ABORTED` and boots the **previous working application**.
   An app can also sentence itself via
   `esp_ota_mark_app_invalid_rollback_and_reboot()`.
4. **What makes an image invalid at push time**: our server-side flow
   (`firmware/esp32-ccid/src/ota.rs`) calls `esp_ota_end()` BEFORE
   `esp_ota_set_boot_partition()`; `esp_ota_end` runs a full
   `esp_image_verify` over the staged bytes: magic byte `0xE9`, segment
   walk, byte checksum, and the appended full-image SHA-256. A corrupt
   or truncated image is rejected there (server replies `'E'`, running
   image untouched) and never reaches the bootloader. The bootloader
   runs the same validation at boot and falls back to the remaining
   bootable slots when the selected image fails it.
5. **otadata** lives at **0x30000 (0x2000)** on this bench
   (`firmware/esp32-ccid/partitions-ota.csv`): two 32-byte sectors with
   an `ota_seq` counter pointing at the boot slot plus the state field
   driving the machine above. There is **no factory partition** on this
   bench (ota_0/ota_1 only) - see "If rollback fails entirely" below.

### Our firmware's hardening

`firmware/esp32-ccid/src/wifi_common.rs::station_bring_up()` orders the
bring-up: WiFi station connect (or visible-AP scan + degraded
`0.0.0.0`), `netlog::set_ip`, `ota::spawn` (the :3232 server), and only
then `esp_ota_mark_app_valid_cancel_rollback()` (rc!=0 logs a warning -
normal on a first boot or non-OTA boot where there is no pending state).
So an image that cannot even bring up its network stack never confirms
itself: the NEXT reset rolls it back. The reader/NFC ladder runs after
the mark, so the mark means "network bring-up succeeded", and the
recovery signals used by this suite (OTA port, vpcd ESTAB, reader ATR)
all sit at-or-after that line.

## Wire protocol (from `firmware/esp32-ccid/src/ota.rs`)

```
connect TCP <board>:3232
-> "OTA1" magic (4 bytes) + image size (u32 LE)
-> raw image bytes (server reads 1 KiB chunks)
<- nothing on success (board switches boot partition, reboots ~0.5 s later;
   the socket dies with the board)
<- single 'E' byte on error (bad magic, bad size, esp_ota_begin/write/end
   failure - including image validation), then close
```

Size limits: 1 byte .. 4 MiB. Host client: `ota_push.py` in this
directory (attributed copy of the bench-proven `/tmp/opencode/idf555/
ota_push.py` client, plus a structured `push_ex()` used by the tests).
`tests/hardware/vpcd/ota_push.py` is the vpcd-suite sibling of the same
client (owned by that suite).

## Defense tiers and the bad-image taxonomy

| id | crafted shape | expected tier | expected outcome |
|---|---|---|---|
| `truncated` | first 60% of the good image | 1 | `'E'` at `esp_ota_end` (segment walk ends mid-image); board never reboots; good image keeps serving |
| `corrupt-tail` | last 4 KiB overwritten `0xFF` | 1 | `'E'` (checksum + appended SHA-256 destroyed); no reboot |
| `checksum-invalid` | 64 B bit-flipped mid-body | 1 | `'E'` (checksum/SHA mismatch); no reboot |
| `never-valid` | valid minimal app, never marks valid | 2 | accepted -> boots -> no confirmation -> next reset = `ABORTED` -> bootloader serves the previous good slot again |

Tier 1 is deterministic for the three crafted kinds (see Research #4),
so `test_ota_rollback.py` asserts the `'E'` rejection AND that the board
stayed healthy throughout. If a crafted image ever slips past tier 1,
the test still demands full recovery within the 150 s window and records
the observed tier in the ledger - the bench invariant ("board serves the
good image before and after") is what is asserted, the mechanism is what
is recorded.

### Case d (never-valid) - documented follow-up

Building the case-d image needs a real (tiny) esp-idf app: valid image,
no WiFi/OTA/vpcd bring-up, infinite idle loop (or a deliberate
bootloop), never calling `esp_ota_mark_app_valid_cancel_rollback`. That
is a firmware build, out of scope for this authoring task (and the
firmware sources are off-limits here). Design for the follow-up:

- `idf.py`-built minimal app (~1 KB image); serve its `.bin` via
  `NUCULA_OTA_NEVERVALID_BIN` - the test
  (`test_never_valid_image_rolls_back`) is already authored and opt-in.
- An idling never-valid app has no network, so nothing can reset it
  remotely: the reset that triggers rollback is either the image
  bootlooping by itself, or ONE bench power-cycle (the test prints when;
  nucula: wall adapter). A bootlooping variant (crash after N seconds)
  is preferred: no human in the loop.
- Cheap approximation to build first: any valid image + `esp_ota_mark_
  app_invalid_rollback_and_reboot()` wired to a timer - same state
  machine, self-sentenced.

### m5stick caveat (bench audit 2026-10-13)

Since 94cdf20 EVERY OTA-slot .full config (incl. m5stick's
`sdkconfig-xtensa-vpcd.full`) sets `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`.
CAVEAT THAT REMAINS: the option is baked into the COMPILED BOOTLOADER —
until the m5stick's bootloader is reflashed, the knob is present in
config but not enforced by the on-board bootloader. The suite now
treats m5stick as rollback_hardened=True (config-side coherence)
- `test_never_valid_image_rolls_back` skips on `--board=m5stick` - and
  the tier-1 tests remain valid there (esp_ota_end validation is
  independent of the rollback option). Do not "fix" this by editing the
  sdkconfig casually: flashing a new bootloader is a bench decision.

### Rollback wiring map (verified 2026-10-25, t_a11d2275 software half)

Static map of knob (sdkconfig) x mark-valid (main), locked by
`test_wiring_coherence.py` (local, stdlib-only):

| build.sh board | sdkconfig | knob | main | marks valid? |
|---|---|---|---|---|
| `wifi-vpcd` (nucula seat) | `sdkconfig.full` | **y** | `wifi_vpcd` -> `station_bring_up` | yes |
| `wifi-ccid` (legacy) | `sdkconfig.full` | **y** | `wifi_ccid` -> `wifi_common::mark_app_valid` | yes (since t_a11d2275; previously NEVER - a good image pushed over the OTA seat reverted on the next reset) |
| `c3` / `c3-ccid` | `sdkconfig.full` | **y** | `pn7160_bringup` / `pn7160_ccid` | **no - tier-2 revert-by-design when pushed** (no network-confirm contract; `c3`'s cred-conditional `ota::spawn` is a debug convenience) |
| `m5stick-vpcd` (m5stick seat) | `sdkconfig-xtensa-vpcd.full` | not set | `wifi_vpcd` -> `station_bring_up` | call present but INERT (see m5stick caveat above) |
| all other boards | xtensa/c3-ble `.full`s | not set | UART/BLE/SPP mains | no (no OTA surface) |

Mechanical note for diagnosis readers: a revert on ANY board proves
that board's bootloader has the knob ON (a knob-off bootloader has no
pending-verify state machine and can never revert anything). An OTA'd
nucula image that reverts therefore means the image never confirmed -
either it is a never-mark main (rows 3), or it chip-reset before its
`mark_app_valid` (the documented rst:0x15 CDC-open race - see
docs/VPCD_WIFI_RUNBOOK.md "opening the port toggles DTR and
chip-resets the board"), not an inverted config.

## Suite layout

```
tests/hardware/ota/
  ota_push.py            push client library (attributed copy) + push_ex()
  bad_images.py          crafting helpers - pure functions, unit-tested
  conftest.py            gate, --board mapping, probes, ledger, fixtures
  test_bad_images.py     local unit tests for the crafting helpers
  test_ota_push_local.py local unit tests for push_ex() vs a fake board
  test_ota_positive.py   e2e: good image -> board healthy
  test_ota_rollback.py   e2e: taxonomy, mandatory good-image restore
  test_wiring_coherence.py local unit tests - knob x mark-valid wiring map
  results/               gitignored ledger output (ota-history.jsonl)
```

### Running

```sh
# local self-checks (no hardware, no gate)
pytest tests/hardware/ota --collect-only
pytest tests/hardware/ota/test_bad_images.py tests/hardware/ota/test_ota_push_local.py

# bench e2e (nucula; m5stick swaps --board and the port mapping)
NUCULA_OTA_E2E=1 NUCULA_OTA_GOOD_BIN=~/.cargo-target/esp32-ccid-wifi-vpcd.bin \
  pytest tests/hardware/ota/test_ota_positive.py --board nucula

NUCULA_OTA_E2E=1 NUCULA_OTA_GOOD_BIN=<good.bin> \
  pytest tests/hardware/ota/test_ota_rollback.py --board nucula

# case (d), once its image exists
NUCULA_OTA_E2E=1 NUCULA_OTA_GOOD_BIN=<good.bin> \
  NUCULA_OTA_NEVERVALID_BIN=<nevervalid.bin> \
  pytest tests/hardware/ota/test_ota_rollback.py -k never_valid --board nucula
```

Environment: `NUCULA_OTA_E2E=1` (gate), `NUCULA_OTA_GOOD_BIN` (required,
restore image), `NUCULA_OTA_NEVERVALID_BIN` (case d), `--board
nucula|m5stick` (IP + vpcd port + reader-slot expectations:
nucula = 192.168.13.230 / :35963 / "Nucula VPCD 00 00" with the SmartPGP
ATR expected; m5stick = 192.168.13.234 / :35964 / "Nucula VPCD 00 01"
with an empty-reader answer expected), `NUCULA_BOARD_IP` /
`NUCULA_VPCD_PORT` (overrides, shared convention with the vpcd suite),
`NUCULA_OTA_EXPECT_CARD=0|1` (force the ATR assertion),
`NUCULA_OTA_REQUIRE_PING=0` (soften the ICMP signal on filtered benches).

### Recovery signals

A board is "healthy" when all of: OTA server `:3232` accepts TCP (only
the real firmware serves it), a vpcd ESTAB session sits on its listener
port (`/proc/net/tcp`; the board connects OUT - runbook
"Architecture"), and its reader slot answers pcscd via a REAL
`SCardConnect` (runbook "Operational rules" #2; ATR or clean empty).
ICMP ping is asserted too unless softened. Recovery windows are
generous: 150 s (positive + tier-2), 300 s for the case-d power-cycle
flow.

## Ledger

Every run appends to `tests/hardware/ota/results/ota-history.jsonl`
(gitignored) using the same record shape as
`tests/hardware/labgrid/results/history.jsonl` (OpenHTF pattern,
borrowed from bolty-rs): one JSON object per line,
`{"ts": "%Y-%m-%dT%H:%M:%S", "event": ..., "board": ..., ...fields}`,
written best-effort (a ledger failure never fails a run). Events:
`ota_suite_session`, `ota_positive_start/push/pass`,
`ota_negative_start/push/outcome`, `ota_nevervalid_push`,
`ota_restore_ok_already_healthy/ok/push_start/retry`.

Coordination note for `tests/hardware/perf` (authored separately): if
that suite lands a results ledger under a different path, keep the
`{"ts", "event", ...}` record shape and the `results/`-is-gitignored
convention so bench reports can be merged; this suite's path is
`tests/hardware/ota/results/ota-history.jsonl`.

## Safety analysis

**If rollback fails entirely** (both OTA slots bad, otadata corrupt, or
the good slot's image itself broken - e.g. a tier-1 rejection followed
by a flash error, or a bad image that passed validation AND the previous
slot is also unbootable): the bootloader finds no bootable app (no
factory partition on this bench) and the board is stuck at the
bootloader. Because both slots' bad states live in otadata at 0x30000
and the app slots at 0x40000/0x1E0000, recovery is **USB serial
reflash** - the exact `build.sh --flash` incantations (build.sh writes
the partition table together with the app and then erases otadata so
the bootloader lands on the first valid slot):

```sh
cd firmware/esp32-ccid
# nucula (ESP32-C3; ota_0 @ 0x40000; same USB-Serial/JTAG port as console)
NUCULA_WIFI_SSID=<ssid> NUCULA_WIFI_PASS=<pass> NUCULA_VPCD_HOST=<host-ip> \
  ./build.sh wifi-vpcd --flash /dev/serial/by-id/<espressif-jtag-port>
# manual equivalent (what build.sh runs):
#   esptool --chip esp32c3 elf2image --flash-mode dio -o /tmp/c3.bin <target ELF>
#   esptool --chip esp32c3 -p <port> --baud 460800 \
#     write-flash 0x8000 <partition-table.bin> 0x40000 /tmp/c3.bin
#   esptool --chip esp32c3 -p <port> --baud 460800 erase-region 0x30000 0x2000

# m5stick (ESP32; ota_0 @ 0x40000 with the OTA table; baud 115200)
NUCULA_VPCD_PORT=35964 ./build.sh m5stick-vpcd --flash /dev/serial/by-id/<m5stick-port>
```

Notes from the bench: opening the USB serial port toggles DTR and
chip-resets the board (runbook "Firmware build + flash"); after an
esp32 flash, physically replug the M5Stack board before expecting serial
to work again (FTDI DTR/RTS wedge, see `flash_and_test.sh` header). An
un-stuck that avoids reflashing an app: `otatool.py --port <port>
erase_otadata` (IDF docs, "otatool") resets slot selection - usable when
one slot is still good but otadata points at the bad one. Last resort
`esptool erase_flash` wipes EVERYTHING including NVS (WiFi credentials)
- full reflash required afterwards. The restore fixture in
`test_ota_rollback.py` re-pushes the good image up to 3 times before
giving up and pointing here.

**The SmartPGP card is never touched by OTA tests.** The only card-level
interaction is the reader probe: a PC/SC `SCardConnect` (card
activation, reading the ATR) on the board's virtual reader slot. No
APDU is transmitted, no SELECT, no GPG/GP path is exercised - this
suite cannot modify, wipe, or re-install anything on the card. The
card's presence is an INPUT (an expected ATR on the nucula slot), not a
test subject.

## Not covered

- Secure boot / flash encryption / anti-rollback eFuse versioning
  (`CONFIG_BOOTLOADER_APP_ANTI_ROLLBACK`) - not enabled on this bench.
- Power loss DURING esp_ota_write (partial slot write): the staging
  slot + esp_ota_end validation cover it logically; a bench power-cut
  rig would be needed to exercise it physically.
- Wi-Fi drop mid-transfer: socket timeout in the client, slot never
  switched (esp_ota_end never called on a short stream).
