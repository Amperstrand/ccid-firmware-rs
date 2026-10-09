# CCID Firmware HIL Testing

Hardware-in-the-loop tests for the CCID reader firmware: the STM32F469
DUT, the two ESP32 serial DUTs (nucula, m5stick), and the reference
readers — coordinated through labgrid places.

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
```

## Test topology per place

| Place | Identity probe | Suite |
|---|---|---|
| `stm32-ccid` | pcscd `ST2XXX-001` (Cherry emul) | `test_ccid_hil.py` (USB enum, ATR, APDU, pinpad, escape) |
| `nucula-c3` | GemPC Escape 0x02 over USB-CDC | `test_serial_battery_hil.py` |
| `m5stick` | GemPC Escape 0x02 over UART0 | `test_serial_battery_hil.py` |
| `ref-acr1252` | pcscd `ACR1252` | identity only (reference oracle) |
| `ref-cardman` | pcscd `CardMan` | identity only (contact reference) |

## Testbed topology

Single bench host (ai-legion, .208) — tests run ON it as local
subprocesses (labgrid doctrine rule 3). There is no SSH anywhere in the
harness (the pre-labgrid SSH style was removed in the 2026-10-09
migration); labgrid coordinates the places, and the exporter runs on the
bench host from the in-repo config (`exporter-ai-legion-nfc.yaml`,
deployed to `/etc/labgrid/exporter-nfc.yaml`).

```
bench host (ai-legion .208)
├── labgrid-exporter ── 5 places (see table above)
├── ST-LINK/V2.1 ──▶ STM32F469  (USB CCID, Cherry ST-2xxx emul)
├── FTDI         ──▶ M5Stick    (UART0 GemPC Twin serial)
├── USB-JTAG     ──▶ nucula     (USB-CDC GemPC Twin serial)
└── ACR1252 · CardMan  (reference readers, never DUTs)
```

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
| `place_name` | function | Parametrized place acquire/release; busy → skip with holder |
| `labgrid_place` | session | Session hold of `stm32-ccid` (classic STM32 suite) |
| `bench` | session | Local host command runner (no SSH) |
| `flashed_firmware` | session | Flashes `.bin` via `st-flash --reset write` (local) |
| `cherry_reader` | session | Verifies Cherry ST-2xxx (046A:003E) in `lsusb` |
| `pcscd_running` | session | Ensures `pcscd.socket` + `pcscd.service` active |
| `pcsc_reader_name` | session | The DUT reader by USB serial (never enumeration order) |
| `remote_apdu(hex)` | function | APDU via local pyscard, returns `(data, sw1, sw2)` |

## Writing new HIL tests

```python
import pytest
from conftest import remote_apdu

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
| Place skip: "held by ..." | Another session owns the board — coordinate, don't force |
| Bench skip: "bench flock held" | A cross-project session (bolty/micronuts) is on the bench |
