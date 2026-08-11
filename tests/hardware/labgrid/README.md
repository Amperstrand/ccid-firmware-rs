# CCID Firmware HIL Testing

Hardware-in-the-loop tests for the STM32F469I-DISCO CCID reader firmware.

## Testbed topology

```
┌─────────────────────────────┐        ┌──────────────────────────────┐
│  Build host (.221)          │  SSH   │  HIL host (.208)             │
│  ┌───────────────────────┐  │───────▶│  ┌────────────────────────┐  │
│  │ pytest + conftest.py  │  │        │  │ st-flash + pcscd       │  │
│  │ cargo build (release) │  │        │  │ pyscard                │  │
│  │ labgrid-client        │  │        │  │ labgrid-exporter       │  │
│  └───────────────────────┘  │        │  └────────────────────────┘  │
└─────────────────────────────┘        │                               │
                                       │  ST-LINK/V2.1 ──▶ STM32F469  │
                                       │  USB CCID ◀──── Cherry ST-2x │
                                       │  Contact slot: ComSign eID   │
                                       └──────────────────────────────┘
```

## Prerequisites

### HIL host (192.168.13.208)

```bash
# st-link tools (already installed)
apt install stlink-tools

# pcscd + opensc
apt install pcscd pcsc-tools opensc libpcsclite-dev

# python deps
pip3 install --break-system-packages --ignore-installed typing_extensions pyscard labgrid
```

### Build host (192.168.13.221)

```bash
# Rust + ARM target (already set up via rust-toolchain.toml)
rustup target add thumbv7em-none-eabihf

# Python test deps
pip3 install pytest pyscard

# SSH key auth to HIL host
ssh-copy-id root@192.168.13.208
```

## Running HIL tests

### Quick smoke (flash + enumerate + ATR + APDU)

```bash
# Build the firmware
cargo build --release --target thumbv7em-none-eabihf

# Convert to .bin
arm-none-eabi-objcopy -O binary \
  target/thumbv7em-none-eabihf/release/ccid-firmware \
  /tmp/ccid-firmware.bin

# Run HIL tests (flash, verify USB, pcscd, ATR, APDU)
pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil \
  --firmware-bin=/tmp/ccid-firmware.bin
```

### Run against current flash contents (no reflash)

```bash
pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil
```

### CI mode (HIL tests skipped)

```bash
pytest tests/hardware/labgrid/ -v
# All tests show SKIPPED unless --hil is passed.
```

## Available test fixtures

| Fixture | Scope | Description |
|---------|-------|-------------|
| `remote` | session | `RemoteHost` wrapping SSH access to .208 |
| `flashed_firmware` | session | Flashes `.bin` via `st-flash --reset write` over SSH |
| `cherry_reader` | session | Verifies Cherry ST-2xxx (046A:003E) in `lsusb` |
| `pcscd_running` | session | Ensures `pcscd.socket` + `pcscd.service` active |
| `pcsc_reader_name` | session | Returns the reader name from pyscard |
| `remote_apdu(hex)` | function | Sends an APDU via remote pyscard, returns `(data, sw1, sw2)` |

## Writing new HIL tests

```python
import pytest
from conftest import remote_apdu

pytestmark = pytest.mark.hil  # skip unless --hil

def test_my_scenario(pcsc_reader_name, cherry_reader):
    data, sw1, sw2 = remote_apdu(cherry_reader, "00 A4 04 00 04 A0 00 00 00 62")
    assert sw1 in (0x90, 0x6A, 0x6E)
```

## Labgrid exporter (optional — for resource locking)

The pytest harness above uses direct SSH. For multi-user / multi-testbed
setups, start the labgrid exporter on .208 to enable resource locking:

```bash
# On .208
labgrid-exporter tests/hardware/labgrid/exporter-208.yaml &

# On .221
export LG_ENV=tests/hardware/labgrid/environment.yaml
labgrid-client resources   # verify exporter is reachable
```

The exporter matches the ST-LINK/V2.1 by USB VID:PID (0483:374b) and exports
SSH access so labgrid clients can acquire the testbed exclusively.

## Teardown / cleanup

After HIL tests, if you need to restore a different firmware (e.g.
gm65-scanner) to the STM32:

```bash
ssh root@192.168.13.208 \
  'st-flash --reset write /path/to/gm65-firmware.bin 0x08000000'
```

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `st-flash` fails to connect | Power-cycle the STM32 board, retry |
| Cherry reader not in `lsusb` after flash | Wait 3s for USB re-enum, or reset USB PHY (issue #22) |
| `pcsc_scan` shows no readers | `systemctl start pcscd.socket pcscd.service` |
| pyscard `Unsupported card` from opensc | Use pyscard directly (bypasses opensc driver matching) |
| SSH timeout | Verify host is up: `ping 192.168.13.208` |
