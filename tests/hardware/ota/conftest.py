"""Fixtures + gates for the OTA update/rollback hardware suite.

Everything encodes:
- docs/VPCD_WIFI_RUNBOOK.md (board IPs, vpcd listener ports, reader
  slot names, recovery signals, "board connects OUT" architecture)
- firmware/esp32-ccid/src/ota.rs (wire protocol, 'E' rejection)
- firmware/esp32-ccid/src/wifi_common.rs (mark-valid-after-bring-up)
- ESP-IDF OTA Update Pipeline semantics (README.md "Research" for the
  citations): pending-verify state, one boot attempt, otadata @ 0x30000.

Gating (README "Running"):
- The whole e2e suite is skipped unless NUCULA_OTA_E2E=1.
- Unit tests marked ``ota_unit`` (test_bad_images.py,
  test_ota_push_local.py) always run - no hardware, no pyscard needed.
- NUCULA_OTA_GOOD_BIN must point at the known-good app image; it is the
  mandatory restore image AFTER every negative test.
"""

import json
import os
import socket
import subprocess
import time
from pathlib import Path

import pytest

#: firmware/esp32-ccid/src/ota.rs
OTA_PORT = 3232

#: Bench boards (task brief + runbook "Multi-board benches"). The
#: stock libifdvpcd driver names both readers "Nucula VPCD ..."; the
#: slot number follows the listener port (:35963 = 00 00, :35964 = 00 01).
#:
#: rollback_hardened reflects the tracked sdkconfigs at the time of
#: writing (audit note in README "m5stick caveat"):
#:   nucula  firmware/esp32-ccid/sdkconfig.full:1769
#:     CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y
#:   m5stick firmware/esp32-ccid/sdkconfig-xtensa-vpcd.full:288
#:     # CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE is not set
#: Tier-1 (device-side rejection) tests are valid on both boards; the
#: tier-2 never-valid rollback test requires rollback_hardened=True.
BOARDS = {
    "nucula": {
        "ip": "192.168.13.230",
        "vpcd_port": 35963,
        "reader_name": "Nucula VPCD 00 00",
        "chip": "esp32c3",
        "flash_offset": "0x40000",
        "flash_baud": "460800",
        # nucula bench card is the SmartPGP (task brief; wall power).
        "expect_card": True,
        "rollback_hardened": True,
    },
    "m5stick": {
        "ip": "192.168.13.234",
        "vpcd_port": 35964,
        "reader_name": "Nucula VPCD 00 01",
        "chip": "esp32",
        "flash_offset": "0x40000",
        "flash_baud": "115200",
        # Bench 2026-10-13 (wifi_common.rs MFRC522 note): reader frontend
        # degrades to an EMPTY reader - the slot must still ANSWER pcscd,
        # but no ATR is expected.
        "expect_card": False,
        "rollback_hardened": False,
    },
}

#: Recovery windows. The brief demands 120 s+; 150 s covers board
#: reboot + WiFi join + vpcd reconnect + a slow DHCP renewal.
RECOVERY_TIMEOUT_S = 150
PROBE_INTERVAL_S = 5

_RESULTS_DIR = Path(__file__).parent / "results"
_LEDGER_PATH = _RESULTS_DIR / "ota-history.jsonl"


# ---------------------------------------------------------------------------
# options + markers
# ---------------------------------------------------------------------------


def pytest_addoption(parser):
    parser.addoption("--board", choices=sorted(BOARDS), default="nucula",
                     help="bench board under test (default: nucula)")


def pytest_configure(config):
    config.addinivalue_line(
        "markers", "ota_unit: local unit test - runs without the "
        "NUCULA_OTA_E2E gate and without hardware")


@pytest.fixture(autouse=True)
def _e2e_gate(request):
    """CI/other benches skip the e2e surface unless the bench opt-in is
    set; ``ota_unit`` tests are exempt."""
    if request.node.get_closest_marker("ota_unit"):
        return
    if os.environ.get("NUCULA_OTA_E2E") != "1":
        pytest.skip("NUCULA_OTA_E2E != 1 - OTA hardware bench suite disabled")


# ---------------------------------------------------------------------------
# board + images
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def board(request):
    """Bench endpoint set for --board, with env overrides shared with the
    vpcd suite (NUCULA_BOARD_IP / NUCULA_VPCD_PORT)."""
    cfg = dict(BOARDS[request.config.getoption("--board")])
    cfg["ip"] = os.environ.get("NUCULA_BOARD_IP", cfg["ip"])
    cfg["vpcd_port"] = int(os.environ.get("NUCULA_VPCD_PORT",
                                          str(cfg["vpcd_port"])))
    # Card expectation is a bench variable (m5stick serves an empty reader;
    # a card may or may not be coupled). 1/0 forces the ATR assertion.
    forced = os.environ.get("NUCULA_OTA_EXPECT_CARD")
    if forced is not None:
        cfg["expect_card"] = forced == "1"
    return cfg


def _image_from_env(varname, required_hint):
    path = os.environ.get(varname, "")
    if not path:
        pytest.skip(f"{varname} not set - {required_hint}")
    if not os.path.isfile(path):
        pytest.fail(f"{varname}={path!r} is not a readable file")
    return path


@pytest.fixture(scope="session")
def good_bin():
    """The known-good app image (esp-idf .bin, not the raw ELF). Required
    for every e2e test: it is both the positive-test payload and the
    mandatory restore image after negative tests."""
    return _image_from_env(
        "NUCULA_OTA_GOOD_BIN",
        "point it at the known-good board image (the restore image)")


@pytest.fixture(scope="session")
def nevervalid_bin():
    """Optional taxonomy case (d): a MINIMAL valid image that boots and
    idles without ever calling esp_ota_mark_app_valid_cancel_rollback.
    See README 'Case d' for how to build one."""
    return _image_from_env(
        "NUCULA_OTA_NEVERVALID_BIN",
        "case (d) never-valid image not provided (documented follow-up)")


@pytest.fixture
def craft_bad_image(good_bin, tmp_path, request):
    """Factory: craft a taxonomy case from the good image, keep a forensic
    copy under tmp_path, return (kind, path)."""
    def _craft(kind):
        from .bad_images import BAD_IMAGE_KINDS, craft

        data = Path(good_bin).read_bytes()
        bad = craft(kind, data)
        path = tmp_path / f"bad-{kind}.bin"
        path.write_bytes(bad)
        assert len(bad) != len(data) or bad != data, "craft was a no-op"
        return BAD_IMAGE_KINDS[kind], path

    return _craft


# ---------------------------------------------------------------------------
# ledger - same JSONL record shape as tests/hardware/labgrid/results/
# history.jsonl ({"ts", "event", **fields}, OpenHTF pattern). See README
# "Ledger" for the coordination note with tests/hardware/perf.
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def ledger(board):
    def _ledger(event, **fields):
        """Append one run record. Never raises - a ledger failure must not
        fail a bench run (results/ is gitignored bench state)."""
        try:
            _RESULTS_DIR.mkdir(parents=True, exist_ok=True)
            record = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"),
                      "event": event, "board": board["ip"]}
            record.update(fields)
            with _LEDGER_PATH.open("a") as f:
                f.write(json.dumps(record) + "\n")
        except OSError:
            pass

    _ledger("ota_suite_session", board_sel=board["ip"],
            vpcd_port=board["vpcd_port"], pid=os.getpid())
    return _ledger


# ---------------------------------------------------------------------------
# recovery signal probes (README "Recovery signals")
# ---------------------------------------------------------------------------


def ping_ok(ip):
    """ICMP reachability. Best-effort signal: assertable by default,
    soften with NUCULA_OTA_REQUIRE_PING=0 if a bench filters ICMP."""
    try:
        return subprocess.run(["ping", "-c", "1", "-W", "2", ip],
                              capture_output=True, timeout=5).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def ota_port_up(ip):
    """The OTA TCP server itself - the strongest single signal: only the
    real firmware serves :3232."""
    try:
        with socket.create_connection((ip, OTA_PORT), timeout=2):
            return True
    except OSError:
        return False


def vpcd_estab(port):
    """Any ESTAB session on the board's vpcd listener (board connects
    OUT - runbook "Architecture"). /proc/net/tcp parse, no ss dependency
    (same approach as tests/hardware/vpcd/ota_push.py; port-only match:
    each listener port maps to exactly one board on this bench)."""
    try:
        with open("/proc/net/tcp") as f:
            for line in f.readlines()[1:]:
                parts = line.split()
                if len(parts) < 4 or parts[3] != "01":  # 01 = ESTABLISHED
                    continue
                if int(parts[1].split(":")[1], 16) == port:
                    return True
    except (OSError, ValueError, IndexError):
        pass
    return False


def reader_atr(name):
    """REAL SCardConnect against the board's reader slot (runbook
    "Operational rules" #2 - never trust GetStatusChange caches).

    Returns (slot_answered, atr_hex): (True, "<hex>") when the card
    answered, (True, "") for a live but empty reader, (False, "") when
    the slot did not answer at all / pyscard is unavailable. No APDU is
    ever transmitted - card activation (ATR) only, so the SmartPGP card
    is observed but never touched.
    """
    try:
        from smartcard.Exceptions import (CardConnectionException,
                                          NoCardException)
        from smartcard.System import readers
    except ImportError:
        return False, ""
    try:
        rs = [r for r in readers() if str(r) == name]
        if not rs:
            return False, ""
        conn = rs[0].createConnection()
        try:
            conn.connect()
            return True, bytes(conn.getATR()).hex().upper()
        except NoCardException:
            return True, ""  # live empty reader
        except CardConnectionException:
            return False, ""
        finally:
            try:
                conn.disconnect()
            except Exception:
                pass
    except Exception:
        return False, ""


def probe(board):
    """One snapshot of all recovery signals."""
    answered, atr = reader_atr(board["reader_name"])
    return {"ping": ping_ok(board["ip"]),
            "ota_port": ota_port_up(board["ip"]),
            "vpcd_estab": vpcd_estab(board["vpcd_port"]),
            "reader_answers": answered,
            "atr_hex": atr}


def board_healthy(board, signals=None):
    """Healthy = OTA server answering AND vpcd session established AND
    the reader slot answering pcscd (ATR or clean empty). Card-presence
    (non-empty ATR) is asserted per-board via expect_card."""
    s = signals if signals is not None else probe(board)
    if not (s["ota_port"] and s["vpcd_estab"] and s["reader_answers"]):
        return False
    if board["expect_card"] and not s["atr_hex"]:
        return False
    return True


def wait_for_healthy(board, timeout_s=RECOVERY_TIMEOUT_S):
    """Poll until healthy; returns the final probe dict (or None)."""
    deadline = time.time() + timeout_s
    last = None
    while time.time() < deadline:
        last = probe(board)
        if board_healthy(board, last):
            return last
        time.sleep(PROBE_INTERVAL_S)
    return None


@pytest.fixture
def require_ping():
    """Strictness hook for the ICMP signal (NUCULA_OTA_REQUIRE_PING=0
    softens it on ICMP-filtered benches)."""
    return os.environ.get("NUCULA_OTA_REQUIRE_PING", "1") == "1"
