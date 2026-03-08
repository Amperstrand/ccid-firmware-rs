# Manual Hardware Tests (Non-Destructive)

These tests are manual and are not executed by CI.

## Safety goals

- Do not modify card contents.
- Do not create, delete, or overwrite SIM profiles.
- Avoid PIN verification attempts unless explicitly requested.

## Supported cards

1. SeedKeeper/Satochip card
2. sysmocom pink SIM card

## Prerequisites

- `pcscd` running
- `pyscard` installed for Python scripts
- Optional for sysmocom SIM checks: `pySim-read.py`

## SeedKeeper read-only smoke test

```bash
python3 tests/hardware/seedkeeper_non_destructive.py
```

What it does:

- Connect reader
- Read ATR
- `SELECT` SeedKeeper AID
- Send a read-only/status APDU and report status words

What it does not do:

- No `VERIFY_PIN`
- No import/export/write APDUs

## sysmocom SIM read-only smoke test

```bash
python3 tests/hardware/sysmocom_sim_non_destructive.py
```

What it does:

- Connect reader
- Read ATR
- Select MF (`3F00`) using read-only APDU

Optional additional read-only check:

```bash
pySim-read.py -p <slot>
```

Avoid running any `pySim-shell` commands that write files, modify ADM settings,
or update profiles.
