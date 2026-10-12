"""STM32 HIL APDU helpers (uniquely-named module — NOT conftest).

Test modules import from here, never `from conftest import ...`: a bare
`conftest` module name is claimed by whichever suite's conftest.py is
imported first when multiple suite directories are collected together
(e.g. `pytest tests/hardware/labgrid/ tests/hardware/nfc/`), which breaks
collection with ImportErrors. Same pattern as bench.py.
"""

import re
import shlex
import subprocess

import pytest

EXPECTED_ATR = "3B D5 18 FF 81 91 FE 1F C3 80 73 C8 21 10 0A"
CARD_CMD_ATTEMPTS = 3
HELPER_DIR = "/tmp/hil-ccid-bench"


def remote_apdu(bench, apdu_hex: str, timeout: int = 15) -> tuple[bytes, int, int]:
    """Send one APDU via the pcscd helper. Contact cards flake (the bench
    ComSign eID is aged — SELECT times out ~1-in-3): retry with a fresh
    connection each attempt."""
    clean = apdu_hex.replace(" ", "").replace("\t", "")
    if not re.match(r'^[0-9A-Fa-f]+$', clean):
        pytest.fail(f"Invalid APDU hex: {apdu_hex!r}")
    last_err = None
    for attempt in range(1, CARD_CMD_ATTEMPTS + 1):
        try:
            result = bench.run(f"python3 {HELPER_DIR}/hil_apdu.py {shlex.quote(clean)}", timeout=timeout)
        except subprocess.TimeoutExpired:
            last_err = f"attempt {attempt}: timeout"
            continue
        if result.returncode == 0 and ":" in result.stdout:
            output = result.stdout.strip()
            data_hex, sw_hex = output.rsplit(":", 1)
            data = bytes.fromhex(data_hex) if data_hex else b""
            sw1, sw2 = int(sw_hex[:2], 16), int(sw_hex[2:4], 16)
            return data, sw1, sw2
        last_err = f"attempt {attempt}: rc={result.returncode} out={result.stdout.strip()[:120]}"
    pytest.fail(f"APDU {clean} failed after {CARD_CMD_ATTEMPTS} attempts ({last_err})")
