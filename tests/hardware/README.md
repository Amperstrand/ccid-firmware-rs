# Manual Hardware Tests (Non-Destructive)

These tests are manual and are not executed in CI.

> Automated bench testing lives in `tests/hardware/labgrid/` (labgrid
> places + pcscd identity doctrine — see its README). The scripts here
> are manual, card-safe checks.

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

## NFC field recorder (ACR1252 oracle evidence)

`tests/hardware/nfc/field_recorder.py` records what the bench ORACLE
reader (ACS ACR1252 PICC) observes into an **append-only JSONL file**
— one event per line, tcpdump-style evidence for issues:

```bash
python3 tests/hardware/nfc/field_recorder.py --out evidence.jsonl \
    --apdu 00A40400023132 --apdu 00CA006E00   # scripted APDUs (opt.)
```

Event kinds: `reader_list` (pcscd listing), `card_atr` (connect+ATR),
`apdu_exchange` (request/response/SW + ms), `poll_presence`
(insert/remove transitions, sampled without powering the card),
framed by `recorder_meta`/`recorder_stop` markers. Every line is
`{"ts", "kind", "reader", "data"}` — the full schema is the module
docstring. The reader is pinned by EXACT pcscd name (default the
ACR1252 PICC slot, `--reader` to override), loops are bounded
(`--interval`/`--duration`/`--max-events`), and SC-class APDUs
(INIT_UPDATE, EXT_AUTH, SC-gated GET DATA) are refused at both the
CLI and the library boundary (AGENTS.md card-handling doctrine).
Hardware is bench-verified separately; unit tests run without
pyscard (duck-typed fakes — `test_field_recorder.py`).
