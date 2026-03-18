# CCID Soak Tests

End-to-end integration tests that run identical operations on both a real
Gemalto PC Twin Reader and the STM32F469 running our Rust firmware, then
compare responses to find behavioral differences.

## Prerequisites

- Two USB CCID readers: one Gemalto (BCF852F0) and one STM32F469 (CT30-001)
- JR3180 smartcards inserted in both readers
- `pcscd` installed (`libccid`, `pcsc-tools`, `pyscard`)
- `sudo` access (for pcscd and USB operations)
- `gh` CLI authenticated (for auto-filing GitHub issues on bugs)
- Python 3.12+ with `pyscard`, `pyusb`, `pysatochip`
- Java 21+ with `gp.jar` at `/tmp/gp.jar` (GlobalPlatformPro)
- CAP files at `/tmp/caps/` (optional, for GP install tests)

## Quick Start

```bash
# Run a single suite
sudo python3 tests/soak/soak_01_smoke.py

# Run all suites sequentially
sudo python3 tests/soak/run_all_soak_tests.py

# Run a specific suite
sudo python3 tests/soak/run_all_soak_tests.py --suite 5

# Run all but skip stress tests
sudo python3 tests/soak/run_all_soak_tests.py --skip 9

# Run with the shell wrapper (handles timeouts)
sudo bash tests/soak/run_soak_tests.sh
```

## Test Suites

| # | Script | Name | Tests | CCID Features |
|---|--------|------|-------|---------------|
| 01 | `soak_01_smoke.py` | Smoke test | 6 | ATR, SELECT, power cycle, GET RESPONSE, ENVELOPE |
| 02 | `soak_02_globalplatform.py` | GlobalPlatform | 8 | Extended APDUs, CAP install/delete, SCP |
| 03 | `soak_03_satochip.py` | SatoChip | 5 | ECDSA, PIN verify, BIP32, signing |
| 04 | `soak_04_openpgp.py` | OpenPGP card | 7 | OpenPGP AID, GET DATA, GPG card ops |
| 05 | `soak_05_piv.py` | PIV | 10 | PIV AID, CHUID, CCC, PIN, discovery |
| 06 | `soak_06_opensc.py` | OpenSC toolchain | 10 | opensc-tool, pkcs15-tool, pkcs11-tool |
| 07 | `soak_07_gpg.py` | GPG card ops | 9 | card-status, card-edit, key listing |
| 08 | `soak_08_extended_apdu.py` | Extended APDU | 11 | 100-255 byte APDUs, chaining, mixed CLA |
| 09 | `soak_09_stress.py` | Stress repeat | 7 | 50x connect/disconnect, 50x APDU, 20x power cycle |
| 10 | `soak_10_cross_compare.py` | Cross-reader | 14 | Identical ops on both readers, response comparison |

## How It Works

Each test function runs on both readers sequentially:

1. **Gemalto reader** (BCF852F0) — reference device
2. **Firmware reader** (CT30-001) — STM32F469 running our CCID firmware

The `soaklib.py` `run_test_on_both()` helper:
- Runs the test function on each reader
- Compares success/failure status (real bug if one passes and other fails)
- Compares response content (configurable normalization)
- On behavioral difference: auto-files a GitHub issue

### Comparison Modes

- **Default**: response strings must match exactly
- **`normalize=fn`**: strip reader-specific data before comparing
- **`expect_response_diff=True`**: log differences but don't fail (for card-unique data)

## Known Expected Differences

These are NOT firmware bugs — they're physical differences between the two setups:

- **Card-unique data**: GET DATA 00CF returns card serial numbers that differ
- **Reader serials**: GlobalPlatform output includes the reader name
- **GPG paths**: gpg output references GNUPGHOME paths

These are handled with `normalize` callbacks or `expect_response_diff=True`.

## Logs

All logs are stored in `/home/ubuntu/gt/ccid_firmware/soak-test-logs/` (not committed):

```
soak-test-logs/
  soak-01-smoke/
    summary.json        # Machine-readable results
    summary.txt          # Human-readable results
    pcscd.log            # Raw pcscd debug output (when enabled)
  soak-02-globalplatform/
    ...
  run.log                # Shell wrapper log
  final-report.txt       # Overall summary
  final-report.json      # Machine-readable overall results
```

## Soak Test Results — 2026-03-17

**Firmware commit**: `74dc360`
**Duration**: ~15 minutes (serial execution)
**Result**: **87/87 tests passed, 0 real bugs found**

| Suite | Tests | Result | Notes |
|-------|-------|--------|-------|
| 01 Smoke | 6 | 6/6 | Identical ATRs, all basic CCID ops match |
| 02 GlobalPlatform | 8 | 8/8 | PIV install/uninstall works, card contents differ (expected) |
| 03 SatoChip | 5 | 5/5 | PIN verify, BIP32 pubkey, signing all work |
| 04 OpenPGP | 7 | 7/7 | All APDUs identical, gpg path differences (expected) |
| 05 PIV | 10 | 10/10 | All PIV ops return identical SWs |
| 06 OpenSC | 10 | 10/10 | Full OpenSC toolchain works identically |
| 07 GPG | 9 | 9/9 | card-status, card-edit, export all work |
| 08 Extended APDU | 11 | 11/11 | Up to 255-byte APDUs, all SWs match |
| 09 Stress | 7 | 7/7 | 50x connect/disconnect, 50x APDU, 20x power cycle |
| 10 Cross-Compare | 14 | 14/14 | 14 identical operations, zero differences |

## Gas Town Orchestration

Tests can be dispatched via Gas Town polecats:

```bash
# Create beads from the rig directory
cd /home/ubuntu/gt/ccid_firmware
bd create --title "Soak test run: $(date)" --type task --label "soak-test"

# Sling to polecat (serial execution)
gt sling <bead-id> ccid_firmware --max-concurrent 1
```

Beads use the `cf-` prefix. See `AGENTS.md` for full context.
