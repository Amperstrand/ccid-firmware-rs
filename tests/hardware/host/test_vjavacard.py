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
    # unsigned copy: the shipped jar is signed, which rejects the shadow
    # javacard.security classes compiled into target/classes
    cp ~/.m2/repository/com/licel/jcardsim/2.2.2/jcardsim-2.2.2.jar \
       ~/.m2/repository/com/licel/jcardsim/2.2.2/jcardsim-2.2.2-unsigned.jar
    zip -d ~/.m2/.../jcardsim-2.2.2-unsigned.jar 'META-INF/*.SF' 'META-INF/*.RSA'
    # target/classes FIRST (shadows), then the unsigned jar
    java -cp target/classes:~/.m2/.../jcardsim-2.2.2-unsigned.jar \
        org.amperstrand.VpcdBridge 127.0.0.1 35973
    # real-applet mode (SmartPGP from the pinned CAP artifact; the .cap is an
    # ant-javacard JAR — APPLET-INF/classes holds the compiled applet):
    unzip -o <SmartPGP.cap> 'APPLET-INF/classes/*' -d smartpgp-classes/
    java -cp target/classes:...-unsigned.jar:smartpgp-classes \
        org.amperstrand.VpcdBridge 127.0.0.1 35973 \
        --applet fr.anssi.smartpgp.SmartPGPApplet D276000124010304AFAF000000000000

    OPERATIONAL: after any pcscd restart the vpcd slot answers the ATR probe
    immediately but C-APDU transmits can stay empty for ~2 minutes — do not
    fail-fast on a freshly restarted pcscd.

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
    if _select_sw(ECHO_AID) == (0x6A, 0x82):
        pytest.skip("bridge not in echo mode (launched with --applet)")
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
    if _select_sw(ECHO_AID) == (0x6A, 0x82):
        pytest.skip("bridge not in echo mode (launched with --applet)")
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


# ---------------------------------------------------------------------------
# GP-surface semantics (2026-10-22 lane): jcardsim executes applets but has
# NO GlobalPlatform card manager / secure channel, so every gp operation that
# needs an ISD fails on SELECT. These tests PIN that contract so a future
# harness change that alters it is caught.
# ---------------------------------------------------------------------------

SMARTPGP_AID = "D276000124010304AFAF000000000000"


def _run_gp(*args):
    return subprocess.run(
        ["gp", "-r", READER_SUBSTR, *args], capture_output=True, text=True, timeout=60
    )


def _select_sw(aid_hex):
    """SELECT aid on a fresh connection, return (sw1, sw2)."""
    conn = _connect()
    try:
        aid = bytes.fromhex(aid_hex)
        _, s1, s2 = conn.transmit([0x00, 0xA4, 0x04, 0x00, len(aid)] + list(aid))
        return s1, s2
    finally:
        conn.disconnect()


def test_gp_list_fails_isd_select_no_card_manager():
    """gp --list: jcardsim has no GP card manager — the ISD SELECT cannot succeed.

    The exact SW of the first (empty-AID) SELECT probe is mode-dependent:
    6A82 with nothing selected (echo mode), 6D00 when an applet is currently
    selected (real-applet mode) — what is pinned is that gp cannot establish
    an ISD and aborts.
    """
    if shutil.which("gp") is None:
        pytest.skip("gp (GlobalPlatformPro) not installed")
    p = _run_gp("-l", "-v", "-d")
    assert p.returncode != 0
    assert "Could not SELECT" in p.stdout + p.stderr


def test_gp_install_fails_no_secure_channel():
    """gp --install of a CAP: same wall — no ISD to INSTALL for load against."""
    if shutil.which("gp") is None:
        pytest.skip("gp (GlobalPlatformPro) not installed")
    p = _run_gp("--install", "whatever.cap", "--bs", "200", "-v", "-d")
    assert p.returncode != 0
    assert "Could not SELECT" in p.stdout + p.stderr


# ---------------------------------------------------------------------------
# SmartPGP real-applet mode (--applet fr.anssi.smartpgp.SmartPGPApplet): the
# pinned CAP artifact is an ant-javacard JAR whose APPLET-INF/classes carry
# the compiled applet; the bridge loads it via the jcardsim loadApplet path.
# Only runs when the bridge was launched in SmartPGP mode (see handoff).
# ---------------------------------------------------------------------------


def _smartpgp_mode() -> bool:
    try:
        return _select_sw(SMARTPGP_AID) != (0x6A, 0x82)
    except Exception:
        return False


def test_smartpgp_get_data_answers():
    """REAL SmartPGP code executing behind the vpcd slot: OpenPGP GET DATA."""
    if not _smartpgp_mode():
        pytest.skip("bridge not in SmartPGP mode (launch with --applet)")
    conn = _connect()
    try:
        aid = bytes.fromhex(SMARTPGP_AID)
        conn.transmit([0x00, 0xA4, 0x04, 0x00, len(aid)] + list(aid))
        data, s1, s2 = conn.transmit([0x00, 0xCA, 0x00, 0x4F])  # AID DO
        assert (s1, s2) == (0x90, 0x00)
        assert bytes(data).hex().upper().startswith("D27600012401")
        data, s1, s2 = conn.transmit([0x00, 0xCA, 0x00, 0x6E])  # AppRelatedData
        assert (s1, s2) == (0x90, 0x00)
        assert len(data) > 100
    finally:
        conn.disconnect()


def test_smartpgp_wrong_pin_6982():
    """Applet-level security logic: VERIFY PW1 without prior status -> 6982."""
    if not _smartpgp_mode():
        pytest.skip("bridge not in SmartPGP mode (launch with --applet)")
    conn = _connect()
    try:
        aid = bytes.fromhex(SMARTPGP_AID)
        conn.transmit([0x00, 0xA4, 0x04, 0x00, len(aid)] + list(aid))
        data, s1, s2 = conn.transmit(
            [0x00, 0x20, 0x00, 0x82, 0x08] + [0x30] * 8
        )
        assert (s1, s2) == (0x69, 0x82)
    finally:
        conn.disconnect()
