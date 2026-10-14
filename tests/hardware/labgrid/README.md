# CCID Firmware HIL Testing

Hardware-in-the-loop tests for the CCID reader firmware: the STM32F469
DUT, the two ESP32 serial DUTs (nucula, m5stick), and the reference
readers — coordinated through labgrid places.

> **Org-wide canon (2026-10-14):** cross-project labgrid practices —
> coordinator topology and exporter fleet, reservation queueing
> (`labgrid-client reserve --wait`, priorities, owner-ETA convention),
> one-device-one-exclusion-domain, and the trusted-network security
> model — live in **conwrt-bench `docs/labgrid-coordination.md`**
> (private, single source of truth). This README covers the
> ccid-specific layer on top of that canon.

## Bench coordination (#65)

Three exclusivity layers, taken in this order (same protocol as bolty-rs,
`docs/labgrid-bench-sharing.md` there):

1. **Bench flock** (`/tmp/amperstrand-bench.lock`, `bench.bench_lock`) —
   excludes cross-project sessions (bolty-rs, micronuts) that share the
   bench. Tests that drive pcscd or serial ports hold it.
2. **labgrid places** (`bench.acquired_place` / the `place_name` fixture) —
   per-place exclusion of other labgrid sessions. Busy places SKIP with
   the holder named instead of failing the run.
3. **Identity probes** (`board_identity.py`) — readers are matched by
   stable identity (USB serial / GemPC version string), never enumeration
   order.

Every session appends to `results/history.jsonl` (bench-local, gitignored).

## Running

```bash
# Everything (state + battery + STM32 suite), all places:
pytest tests/hardware/labgrid/ -v --hil

# One place's identity + its battery leg:
pytest tests/hardware/labgrid/test_bench_state.py -v --hil --places m5stick
pytest tests/hardware/labgrid/test_serial_battery_hil.py -v --hil --places m5stick

# STM32 classic suite (flash optional):
pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil \
  --firmware-bin=/tmp/ccid-firmware.bin

# CI / no hardware: everything skips without --hil.
pytest tests/hardware/labgrid/ -v

# Queue politely for a busy bench (reservation, not poll-acquire):
# blocks until the coordinator allocates the place to you, then the
# suite runs; CI lanes add --reserve-priority -10 to yield.
pytest tests/hardware/labgrid/ -v --hil --places m5stick --reserve
```

## Test topology per place

Eight places (2026-10-14): the five original bench seats plus the F746
DUT and the two ESP32 OTA seats served by the second exporter.

| Place | Exporter | Identity probe | Suite |
|---|---|---|---|
| `stm32-ccid` | ai-legion-nfc | pcscd `ST2XXX-001` (Cherry emul) | `test_ccid_hil.py` (USB enum, ATR, APDU, pinpad, escape) |
| `stm32-f746` | ai-legion-nfc | ST-LINK serial `066EFF…2327` (USB id pending plug) | flash/restore via st-flash (see conftest serial pin) |
| `nucula-c3` | ai-legion-nfc | GemPC Escape 0x02 over USB-CDC | `test_serial_battery_hil.py` |
| `m5stick` | ai-legion-nfc | GemPC Escape 0x02 over UART0 | `test_serial_battery_hil.py` |
| `ref-acr1252` | ai-legion-nfc | pcscd `ACR1252` | identity only (reference oracle) |
| `ref-cardman` | ai-legion-nfc | pcscd `CardMan` | identity only (contact reference) |
| `ccid-nucula` | ai-legion-ccid | OTA TCP `192.168.13.230:3232` | `tests/hardware/vpcd/` + `host-tools/ota_push.py` |
| `ccid-m5stick` | ai-legion-ccid | OTA TCP `192.168.13.234:3232` | OTA reflash (vpcd slot :35964) |

## Testbed topology

Two bench hosts in practice:

- **.208 (ai-legion)** — the USB bench: readers, ST-LINKs, pcscd, both
  labgrid exporters, the coordinator. The classic HIL suites run ON it.
- **.221 (compute)** — no bench USB; sessions here use labgrid for
  place coordination and the network paths (OTA seats, vpcd, netlog).

```
coordinator 192.168.13.208:20408  (LG_COORDINATOR — see below)
├── exporter ai-legion-nfc  (config: tests/hardware/labgrid/exporter-ai-legion-nfc.yaml,
│    deployed to /etc/labgrid/exporter-nfc.yaml on .208)
│    ├── ST-LINK 066FFF…4152 ──▶ STM32F469  (⚠️ detached 2026-10-14 — phantom until replugged)
│    ├── ST-LINK 066EFF…2327 ──▶ STM32F746  (user USB still unplugged)
│    ├── Hades2001 FTDI     ──▶ M5Stick    (UART0 GemPC Twin serial)
│    ├── Espressif USB-JTAG ──▶ nucula     (USB-CDC GemPC Twin serial)
│    └── ACR1252 · CardMan  (reference readers, never DUTs)
└── exporter ai-legion-ccid  (system unit labgrid-exporter-ccid.service,
     config deployed to /etc/labgrid/exporter-ccid.yaml; source of truth:
     tools/hil/labgrid-exporter.yaml — converted 2026-10-14 off the
     ephemeral-worktree path, resources + place acquire re-verified)
     └── ccid-nucula / ccid-m5stick OTA seats (NetworkService :3232 — no
         USBSerialPort matches by default: opening the CDC chip-resets
         the nucula; see the config header). Cross-place exclusion with
         the USB places is coordinator-enforced via the shared
         NetworkService match (both pairs verified 2026-10-14)
```

**Coordinator discovery (the #164 failure class):** labgrid 26.0 has NO
client config file — `labgrid-client` uses `LG_COORDINATOR` or falls
back to `127.0.0.1:20408`. On any host that is not .208 the fallback
fails with "Could not connect to coordinator". Fix (deployed on .221
2026-10-14): `/etc/profile.d/labgrid-coordinator.sh` exporting
`LG_COORDINATOR=192.168.13.208:20408` — replicate on new hosts.

**Exporter-config sync rule:** a bench rewiring that edits the DEPLOYED
`/etc/labgrid/exporter-nfc.yaml` MUST land the same edit in
`tests/hardware/labgrid/exporter-ai-legion-nfc.yaml` in the same
session — `bench_inventory.py` parses the in-repo configs as the source
of truth, and the 2026-10-14 audit found the repo copy two rewirings
stale (F746 missing, dead m5stick-ftdi still present).

## Prerequisites (bench host)

```bash
apt install stlink-tools pcscd pcsc-tools opensc libpcsclite-dev
pip3 install --break-system-packages --ignore-installed \
    typing_extensions pyscard labgrid pytest
rustup target add thumbv7em-none-eabihf   # for STM32 firmware builds
```

## Available test fixtures

| Fixture | Scope | Description |
|---------|-------|-------------|
| `bench_lock` | session | Cross-project flock on `/tmp/amperstrand-bench.lock` |
| `place_name` | function | Parametrized place acquire/release; busy → skip with holder, or queue with `--reserve` |
| `labgrid_place` | session | Session hold of `stm32-ccid`; `--reserve` queues via coordinator reservation |
| `bench` | session | Local host command runner (no SSH) |
| `flashed_firmware` | session | Flashes `.bin` via `st-flash --serial <F469 ST-LINK> --reset write` — adapter pinned from the exporter config (board-crossing guard, 2026-10-14) |
| `cherry_reader` | session | Verifies Cherry ST-2xxx (046A:003E) in `lsusb` |
| `pcscd_running` | session | Ensures `pcscd.socket` + `pcscd.service` active |
| `pcsc_reader_name` | session | The DUT reader by USB serial (never enumeration order) |
| `remote_apdu(hex)` | function | APDU via local pyscard, returns `(data, sw1, sw2)` |

## Writing new HIL tests

```python
import pytest
from hil_helpers import remote_apdu

pytestmark = pytest.mark.hil  # skip unless --hil

def test_my_scenario(pcsc_reader_name, cherry_reader):
    data, sw1, sw2 = remote_apdu(cherry_reader, "00A4040004A00000006203")
    assert sw1 in (0x90, 0x6A, 0x6E)
```

Place-aware tests request `place_name` (auto-parametrized over `--places`)
and `bench_lock` when they drive pcscd or a serial port.

## Teardown / cleanup

To restore different firmware to the STM32 (e.g. gm65-scanner) after a
run — locally on the bench host:

```bash
st-flash --reset write /path/to/gm65-firmware.bin 0x08000000
```

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `st-flash` fails to connect | Power-cycle the STM32 board, retry |
| Cherry reader not in `lsusb` after flash | Wait 3s for USB re-enum, or reset USB PHY (issue #22) |
| `pcsc_scan` shows no readers | `systemctl start pcscd.socket pcscd.service` |
| pyscard `Unsupported card` from opensc | Use pyscard directly (bypasses opensc driver matching) |
| Place skip: "held by ..." | Another session owns the board — coordinate, don't force — or re-run with `--reserve` to queue (`--reserve-timeout-min` caps the wait) |
| Bench skip: "bench flock held" | A cross-project session (bolty/micronuts) is on the bench |

## Bench cards & GlobalPlatform (issue #127)

All five cards are NXP P71D321 silicon (JCOP4 J3R180 family, fab batch 6782).
`gp` = `/usr/local/bin/gp` (GlobalPlatformPro v25.10.20, jar at
`/usr/local/share/ccid-bench/gp.jar`).

| Reader / card | ATR | CPLC serial / pre-pers | GP keys |
|---|---|---|---|
| acr1252 PICC | 3B8580018073C821100E | 50947320 / 1619 | **AUTH triple works** (SmartPGP installed) |
| m5stick coil | 3B8580018073C821100E | 50861320 / 1633 | unknown — keyver FF, SCP02 i=17,06,98,0D |
| Cherry contact | 3BD518FF8191FE1FC38073C821100A | 50942720 / 1647 | unknown — keyver FF, SCP02 i=04,33,F5,DB |
| OmniKey contact | 3BD518FF8191FE1FC38073C821100A | 50972320 / 164B | unknown — keyver FF, SCP02 i=04,33,F5,DB |
| nucula coil | 3B8580018073C821100E | — | unprobed (retry-budget discipline) |

Key material locations: `tests/soak/soak_02_globalplatform.py:32-33`
(AUTH triple + DEFAULT) and `Amperstrand/specter-javacard` AGENTS.md
(J3R180 card dossier). Nothing else exists org-wide (searched: repos,
agent sessions, GitHub org — see #127).

Working invocation (ACR card only):

```bash
gp --reader "ACS ACR1252 Dual Reader [ACR1252 Dual Reader PICC] 02 00" \
   --key-enc 5A9E63D03BADBC2A240FE8F534709EDF \
   --key-mac 7CCC1E79D64FC5FA263B8F2955282998 \
   --key-dek B040703EC3DE23EE8AE4CFB6D632AA80 -l
```

**Fail-fast discipline**: failed SCP attempts consume the card's ISD retry
counter (~5–15 total). Already burned: OmniKey 3, Cherry 2, m5stick 2 —
do NOT blind-retry; obtain keys from the card source first (#127).
Identify a card without burning retries with a raw INITIALIZE UPDATE
(no EXTERNAL AUTH): `SELECT A000000151000000` → `80 50 00 00 08 <8 rand>`
→ parse keyver/SCP params from the response.

GP applet lifecycle template (install → use → uninstall, proven with
TeapotApplet on the ACR card without disturbing the SmartPGP fixture):
see the procedure on #127. CAPs: `gh run download` from
Amperstrand/specter-javacard CI (`cap-files` artifact).
