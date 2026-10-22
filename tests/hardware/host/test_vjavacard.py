"""Tests for the vjavacard bridge: a REAL virtual JavaCard behind vpcd.

#261 tier (b). jcardsim (Java Card 2.2.2 simulator, host JVM) executes
actual applet code behind a vpcd listener — pcscd sees a reader whose
card runs Java: SELECT dispatches by AID inside the card runtime, the
applet's process() builds responses, 6A82 for unknown AIDs.

Host setup (one-time, bench host only — no board, no physical card):

    cp libifdvpcd-amperstrand.so libifdvpcd-amperstrand-vjc.so
    /etc/reader.conf.d/vjavacard:
        FRIENDLYNAME "vJavaCard sim"
        DEVICENAME /dev/null:0x8C85
        LIBPATH .../libifdvpcd-amperstrand-vjc.so
        CHANNELID 0x8C85
    systemctl restart pcscd.socket pcscd

    cd tests/hardware/host/vjavacard && mvn -q package -DskipTests
    java -cp target/vjavacard-0.1.0.jar:~/.m2/.../jcardsim-2.2.2.jar \\
        org.amperstrand.VpcdBridge 127.0.0.1 35973

Wire facts (same as the firmware client and the tier-a table card):
2-byte big-endian length + payload; OFF/ON controls get NO reply (an
empty ack de-syncs the driver's next ATR probe); ATR/RESET answer the
ATR; jcardsim 2.2.2 does not auto-dispatch SELECT — the bridge calls
sim.selectApplet(AID) itself.
"""

import shutil
import subprocess
import time
from pathlib import Path

import pytest

# Collection-safe import (#403 follow-up): CI runners do not carry pyscard
# — module-level `from smartcard...` broke pytest collection on main
# (Interrupted: 1 error during collection). Guard + skipif keeps the
# collect gate green everywhere; the tests themselves only ever run on
# the bench host where pyscard is installed.
try:
    from smartcard.Exceptions import (
        CardConnectionException,
        ListReadersException,
        NoCardException,
    )
    from smartcard.System import readers

    _HAS_SMARTCARD = True
except ImportError:  # pragma: no cover — CI runners
    _HAS_SMARTCARD = False
    CardConnectionException = ListReadersException = NoCardException = (
        readers
    ) = None

pytestmark = pytest.mark.skipif(
    not _HAS_SMARTCARD,
    reason="pyscard not installed — vjavacard tests drive pcscd (bench host only)",
)

BRIDGE_JAR = Path(__file__).parent / "vjavacard" / "target" / "vjavacard-0.1.0.jar"
READER_SUBSTR = "vJavaCard sim 00 00"
BRIDGE_PORT = 35973
ECHO_AID = "A00000015145434F01"


def _bridge_ready() -> bool:
    if not BRIDGE_JAR.exists():
        return False
    try:
        out = subprocess.run(
            ["ss", "-tln"], capture_output=True, text=True, timeout=5
        ).stdout
    except OSError:
        return False
    return f":{BRIDGE_PORT}" in out


pytestmark = pytest.mark.skipif(
    not _bridge_ready(), reason="vjavacard bridge not built/running (host setup)"
)


def _connect():
    rs = [r for r in readers() if READER_SUBSTR in str(r)]
    if not rs:
        pytest.skip("vJavaCard reader slot not configured on this host")
    for _ in range(15):
        try:
            conn = rs[0].createConnection()
            conn.connect()
            return conn
        except (CardConnectionException, NoCardException, ListReadersException):
            time.sleep(0.4)
    pytest.fail("vJavaCard reader never became present")


def test_atr_is_j3r180_shape():
    conn = _connect()
    try:
        atr = bytes(conn.getATR()).hex()
        assert atr.upper() == "3B858131FE458073C8211084"
    finally:
        conn.disconnect()


def test_select_returns_self_marked_fci():
    conn = _connect()
    try:
        aid = bytes.fromhex(ECHO_AID)
        data, s1, s2 = conn.transmit([0x00, 0xA4, 0x04, 0x00, len(aid)] + list(aid))
        assert (s1, s2) == (0x90, 0x00)
        # the vpcd driver folds the status word into the response buffer
        assert bytes(data).hex().upper().endswith("564A415641434152449000")  # marker+SW
        assert ECHO_AID in bytes(data).hex().upper()
    finally:
        conn.disconnect()


def test_applet_code_executes_echo():
    conn = _connect()
    try:
        aid = bytes.fromhex(ECHO_AID)
        conn.transmit([0x00, 0xA4, 0x04, 0x00, len(aid)] + list(aid))
        payload = [0xDE, 0xAD, 0xBE, 0xEF]
        data, s1, s2 = conn.transmit([0x80, 0xAA, 0x00, 0x00, 0x04] + payload)
        assert (s1, s2) == (0x90, 0x00)
        assert list(data) == payload + [0x90, 0x00]  # echo + SW (driver folds SW)
    finally:
        conn.disconnect()


def test_unknown_aid_rejected():
    conn = _connect()
    try:
        data, s1, s2 = conn.transmit([0x00, 0xA4, 0x04, 0x00, 0x04, 0xA0, 0x00, 0x00, 0x02])
        assert (s1, s2) == (0x6A, 0x82)
    finally:
        conn.disconnect()


def test_no_select_yet_is_6d00():
    conn = _connect()
    try:
        data, s1, s2 = conn.transmit([0x80, 0xAA, 0x00, 0x00, 0x00])
        assert (s1, s2) == (0x6D, 0x00)
    finally:
        conn.disconnect()


# keep flake quiet about the import we intentionally re-export for callers
_ = shutil.which("java")
