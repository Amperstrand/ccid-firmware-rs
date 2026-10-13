"""Fixtures + global gate for the vpcd-over-WiFi hardware e2e suite.

Everything here encodes docs/VPCD_WIFI_RUNBOOK.md:
- "Operational rules" #2: reader presence by REAL SCardConnect (ATR or
  exception) — never GetStatusChange, whose cache lies after pcscd
  restarts.
- "Operational rules" #3: two driver slots share the bench; which one is
  live is decided per pcscd restart — discover, don't assume.
- "GP": GP keys come from env vars, never from the repo.
- "Host setup": the only sanctioned pcscd restart sequence.

Plain pytest only (pytest + pyscard; gp/gpg via subprocess). No agent,
no CI: the whole directory is skipped unless NUCULA_VPCD_E2E=1.
"""

import os
import socket
import subprocess
import time

import pytest
from smartcard.Exceptions import CardConnectionException, NoCardException
from smartcard.System import readers

#: vpcd reader names exposed by the stock libifdvpcd reader.conf (runbook
#: "Multi-board benches"): :35963 = "Nucula VPCD 00 00", :35964 = "Nucula VPCD 00 01".
NUCULA_PREFIX = "Nucula VPCD"
#: src/ota.rs: OTA TCP server port ("OTA1" + u32-LE size + image stream).
OTA_PORT = 3232
#: firmware/esp32-ccid/src/netlog.rs: WARN+ lines broadcast to <subnet>.255.
NETLOG_PORT = 4567


@pytest.fixture(autouse=True, scope="session")
def _e2e_gate():
    """CI/other benches skip cleanly unless the bench opt-in is set."""
    if os.environ.get("NUCULA_VPCD_E2E") != "1":
        pytest.skip("NUCULA_VPCD_E2E != 1 — vpcd hardware bench suite disabled")


# ---------------------------------------------------------------------------
# bench endpoints
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def board_ip():
    """IP of the nucula board under test (default: the lab bench board)."""
    return os.environ.get("NUCULA_BOARD_IP", "192.168.13.230")


@pytest.fixture(scope="session")
def vpcd_port():
    """Host listener port of the board's vpcd slot (runbook: 35963 = slot 00 00)."""
    return int(os.environ.get("NUCULA_VPCD_PORT", "35963"))


# ---------------------------------------------------------------------------
# reader discovery — runbook "Operational rules" #2 + #3
# ---------------------------------------------------------------------------


def real_connect(name, tries=3):
    """REAL SCardConnect against one reader (runbook rule 2).

    Returns ATR bytes (b"" when the slot is a live but empty reader),
    raises NoCardException/CardConnectionException per the PC/SC contract.
    Never consults GetStatusChange: its cached presence lies after pcscd
    restarts.
    """
    last = None
    for _ in range(tries):
        rs = [r for r in readers() if str(r) == name]
        if not rs:
            raise CardConnectionException(f"reader {name!r} not listed")
        conn = rs[0].createConnection()
        try:
            conn.connect()
            return bytes(conn.getATR())
        except NoCardException:
            return b""
        except CardConnectionException as e:  # dead slot: retry once
            last = e
            time.sleep(2)
        finally:
            try:
                conn.disconnect()
            except Exception:
                pass
    raise last


@pytest.fixture(scope="session")
def slot_map():
    """Map of every Nucula vpcd slot -> ATR hex ('' = live empty reader).

    Discovery is real-connect only (runbook rule 2); retries cover the
    pcscd restart window and the board's 3 s vpcd reconnect cadence.
    """
    deadline = time.time() + 90
    last_err = None
    while time.time() < deadline:
        try:
            names = [str(r) for r in readers() if str(r).startswith(NUCULA_PREFIX)]
            if names:
                out = {}
                for n in names:
                    try:
                        out[n] = real_connect(n).hex().upper()
                    except NoCardException:
                        out[n] = ""
                    except CardConnectionException as e:
                        last_err = e
                if out:
                    return out
        except Exception as e:  # pcscd down / SCardListReaders failed
            last_err = e
        time.sleep(3)
    pytest.fail(f"no Nucula vpcd slot answered within 90s (last: {last_err})")


@pytest.fixture(scope="session")
def card_slot(slot_map):
    """The Nucula slot that really holds the card: (name, reader_index, atr_hex).

    reader_index is the position in full SCardListReaders order — the
    number gpg's scdaemon.conf `reader-port` wants (runbook "GPG").
    """
    for name, atr in slot_map.items():
        if atr:
            idx = [str(r) for r in readers()].index(name)
            return name, idx, atr
    pytest.fail(f"no Nucula slot holds a card (slot_map={slot_map}); "
                "expected the SmartPGP bench card on the reader")


# ---------------------------------------------------------------------------
# GlobalPlatform keys — env only (runbook "GP": "Keys come from env vars —
# never from the repo")
# ---------------------------------------------------------------------------

GP_KEY_ENV = ("NUCULA_GP_KEY_ENC", "NUCULA_GP_KEY_MAC", "NUCULA_GP_KEY_DEK")


@pytest.fixture(scope="session")
def gp_keys():
    missing = [n for n in GP_KEY_ENV if not os.environ.get(n)]
    if missing:
        pytest.skip(f"GP keys missing from env: {', '.join(missing)}")
    return {n: os.environ[n] for n in GP_KEY_ENV}


# ---------------------------------------------------------------------------
# netlog listener (runbook "Logs": UDP :4567 broadcast, WARN+ only)
# ---------------------------------------------------------------------------


@pytest.fixture
def netlog_sock():
    """UDP listener on the netlog broadcast port. Fails with a hint when an
    operator's own netlog tail already holds :4567."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    try:
        s.bind(("0.0.0.0", NETLOG_PORT))
    except OSError as e:
        pytest.fail(f"cannot bind :{NETLOG_PORT}/udp ({e}) — stop any other "
                    "netlog listener (runbook 'Logs' one-liner) and rerun")
    yield s
    s.close()


# ---------------------------------------------------------------------------
# recovery helpers — only for failures caused by stale host state
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def restart_pcscd():
    """Runbook "Host setup" pcscd restart sequence (order matters).

    Use ONLY when a failure is caused by stale pcscd state (cached slot
    presence, stale exclusive scdaemon lock, half-open vpcd ESTAB after a
    board reboot). Never `pkill -9 scdaemon` (runbook rule 1).
    """

    def _restart():
        subprocess.run(["sudo", "-n", "systemctl", "stop",
                        "pcscd.service", "pcscd.socket"],
                       check=True, timeout=30)
        subprocess.run(["sudo", "-n", "rm", "-f", "/run/pcscd/pcscd.comm"],
                       check=True, timeout=10)
        subprocess.run(["sudo", "-n", "systemctl", "start", "pcscd.socket"],
                       check=True, timeout=30)
        time.sleep(1)
        subprocess.run(["sudo", "-n", "systemctl", "start", "pcscd.service"],
                       check=True, timeout=30)

    return _restart


@pytest.fixture(scope="session")
def vpcd_estab(board_ip, vpcd_port):
    """True when an ESTAB TCP session exists between board and its vpcd slot
    listener (board connects OUT — runbook "Architecture")."""

    def _estab():
        out = subprocess.run(["ss", "-tn", "state", "established"],
                             capture_output=True, text=True, timeout=10)
        return any(f":{vpcd_port} " in line and board_ip in line
                   for line in out.stdout.splitlines())

    return _estab
