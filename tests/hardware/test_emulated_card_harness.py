#!/usr/bin/env python3
"""Emulated-card harness tests: the full #157 IFD-driver chain, host-only.

Chain under test (no pcscd, no hardware — runs on any host with gcc +
PCSC headers; the pcscd-level equivalent needs `make smoke`, which
requires root + private mount namespaces and only runs on the bench
host):

    ctypes -> libifd-nucula.so IFDH entry points -> TCP GemPC framing
           -> mock_board.py (card profile) -> scripted picc

Design provenance: vsmartcard splits reader emulation (vpcd, an
ifdhandler .so) from card emulation (vpicc's pluggable SmartcardOS) —
here mock_board.py + card_profiles.py play vpcd+vpicc, and this test
drives the same IFDH entry points pcscd would (IFDHCreateChannelByName,
IFDHPowerICC, IFDHSetProtocolParameters, IFDHTransmitToICC,
IFDHICCPresence, IFDHControl), mirroring vsmartcard's own
`handler_test` card type and ifd-vpcd's self-test approach.

Run: python3 -m pytest tests/hardware/test_emulated_card_harness.py -v
"""
from __future__ import annotations

import ctypes
import os
import selectors
import socket
import subprocess
import sys
import time
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
IFD_DIR = REPO_ROOT / "host-tools" / "ifd-nucula"
SO_PATH = IFD_DIR / "libifd-nucula.so"
MOCK_PATH = IFD_DIR / "mock_board.py"

# ifdhandler.h / reader.h constants (checked against /usr/include/PCSC)
IFD_SUCCESS = 0
IFD_POWER_UP = 500
IFD_POWER_DOWN = 501
IFD_ICC_PRESENT = 615
IFD_ICC_NOT_PRESENT = 616
SCARD_PROTOCOL_T1 = 2
CM_IOCTL_GET_FEATURE_REQUEST = 0x42000000 + 3400
FEATURE_CCID_ESC_COMMAND = 0x13
MAX_CCID_MESSAGE = 271
MAX_ATR_SIZE = 33

G05_ATR = bytes.fromhex("3B858131FE458073C8211084")
EID_ATR = bytes.fromhex("3BD518FF8191FE1FC38073C821100A")
CASE2_APDU = bytes([0xB0, 0xB0, 0x00, 0x00, 0x00])
ZERO_DATA_APDU = bytes([0xB0, 0xB0, 0x00, 0x00, 0x00, 0x00])

DWORD = ctypes.c_ulong
U8P = ctypes.POINTER(ctypes.c_ubyte)


class SCARD_IO_HEADER(ctypes.Structure):
    _fields_ = [("Protocol", DWORD), ("Length", DWORD)]


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Board:
    """mock_board.py subprocess with banner-ready startup."""

    def __init__(self, card: str, port: int | None = None):
        self.port = port or free_port()
        self.proc = subprocess.Popen(
            [sys.executable, str(MOCK_PATH), str(self.port), "--card", card],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        buf = b""
        sel = selectors.DefaultSelector()
        sel.register(self.proc.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + 10.0
        while time.monotonic() < deadline and b"mock board on" not in buf:
            for key, _ in sel.select(0.2):
                buf += key.fileobj.read1(4096) or b""
        sel.close()
        if b"mock board on" not in buf:
            self.stop()
            raise RuntimeError(f"mock board did not start: {buf!r}")

    def stop(self) -> None:
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


LOG_STUB_C = """
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
/* pcscd exports log_msg (-export-dynamic) and the IFD driver resolves
 * it at dlopen time; a standalone loader must supply the same symbol. */
void log_msg(const int priority, const char *fmt, ...)
{
    (void)priority;
    if (!getenv("NUCULA_TEST_LOG"))
        return;
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\\n', stderr);
}
"""


# The compiler the Makefile build will actually use: it honors the
# environment's CC (`CC ?= cc`), so the prereq probe and the stub build
# must invoke the SAME one — with CC=<unavailable compiler> a probe via
# the unrelated hard-coded `cc` succeeds and the subsequent build fails
# the suite instead of taking the documented missing-toolchain skip
# (Codex P2 on #297, issue #435).
CC = os.environ.get("CC", "cc")


def _cc(source: str, out: Path, extra=()):
    return subprocess.run(
        [CC, "-shared", "-fPIC", "-o", str(out), str(source), *extra],
        capture_output=True, text=True)


def _build_driver():
    # Always invoke make — never existence-check the .so. make is
    # mtime-driven, so a source-newer-than-binary tree rebuilds while a
    # fresh one is a near no-op; the harness cannot load a stale driver
    # (Codex P2 on #260). Locked by test_driver_rebuilds_when_source_newer.
    return subprocess.run(["make", "-C", str(IFD_DIR), "libifd-nucula.so"],
                          capture_output=True, text=True)


def _pcsc_prereq_ok(workdir: Path) -> bool:
    """True when the selected compiler (env CC, default cc) + the
    driver's required PCSC headers exist
    (ifdhandler.h AND reader.h — both unconditional includes in
    ifd-nucula.c; debuglog.h is __has_include-optional). cflags use the
    Makefile's own pkg-config / /usr/include/PCSC fallback. Separates
    'optional toolchain missing' (skip) from 'code-caused build
    regression' (fail loudly, Codex P2s on #297)."""
    try:
        cflags = subprocess.run(["pkg-config", "--cflags", "libpcsclite"],
                                capture_output=True, text=True)
        flags = cflags.stdout.split() if cflags.returncode == 0 \
            else ["-I/usr/include/PCSC"]
    except OSError:
        flags = ["-I/usr/include/PCSC"]
    probe = workdir / "prereq.c"
    probe.write_text("#include <ifdhandler.h>\n#include <reader.h>\n"
                     "int main(void) { return 0; }\n")
    try:
        probe_rc = subprocess.run([CC, "-fsyntax-only", *flags, str(probe)],
                                  capture_output=True).returncode
    except OSError:
        return False
    return probe_rc == 0


@pytest.fixture(scope="module")
def driver(tmp_path_factory):
    build_dir = tmp_path_factory.mktemp("ifd-emu")
    if not _pcsc_prereq_ok(build_dir):
        pytest.skip(f"{CC}/PCSC headers unavailable (optional toolchain)")
    build = _build_driver()
    if build.returncode != 0:
        pytest.fail(f"driver build failed with toolchain present: "
                    f"{build.stderr[-400:]}")
    stub_src = build_dir / "logstub.c"
    stub_src.write_text(LOG_STUB_C)
    stub_so = build_dir / "liblogstub.so"
    stub = _cc(stub_src, stub_so)
    if stub.returncode != 0:
        pytest.skip(f"log_msg stub build failed: {stub.stderr[-400:]}")
    ctypes.CDLL(str(stub_so), mode=ctypes.RTLD_GLOBAL)
    lib = ctypes.CDLL(str(SO_PATH))

    lib.IFDHCreateChannelByName.argtypes = [DWORD, ctypes.c_char_p]
    lib.IFDHCreateChannelByName.restype = ctypes.c_long
    lib.IFDHCloseChannel.argtypes = [DWORD]
    lib.IFDHCloseChannel.restype = ctypes.c_long
    lib.IFDHICCPresence.argtypes = [DWORD]
    lib.IFDHICCPresence.restype = ctypes.c_long
    lib.IFDHPowerICC.argtypes = [DWORD, DWORD, U8P, ctypes.POINTER(DWORD)]
    lib.IFDHPowerICC.restype = ctypes.c_long
    lib.IFDHSetProtocolParameters.argtypes = [
        DWORD, DWORD, ctypes.c_ubyte, ctypes.c_ubyte, ctypes.c_ubyte, ctypes.c_ubyte]
    lib.IFDHSetProtocolParameters.restype = ctypes.c_long
    lib.IFDHTransmitToICC.argtypes = [
        DWORD, SCARD_IO_HEADER, U8P, DWORD, U8P,
        ctypes.POINTER(DWORD), ctypes.POINTER(SCARD_IO_HEADER)]
    lib.IFDHTransmitToICC.restype = ctypes.c_long
    lib.IFDHControl.argtypes = [
        DWORD, DWORD, U8P, DWORD, U8P, DWORD, ctypes.POINTER(DWORD)]
    lib.IFDHControl.restype = ctypes.c_long
    return lib


class Session:
    """One board + one driver channel; teardown releases both."""

    def __init__(self, lib, card: str):
        self.lib = lib
        self.board = Board(card)
        devname = f"127.0.0.1:{self.board.port}".encode()
        rc = lib.IFDHCreateChannelByName(0, devname)
        assert rc == IFD_SUCCESS, f"create channel rc={rc}"

    def power_up_atr(self) -> bytes:
        atr = (ctypes.c_ubyte * MAX_ATR_SIZE)()
        n = DWORD(MAX_ATR_SIZE)
        rc = self.lib.IFDHPowerICC(0, IFD_POWER_UP, atr, ctypes.byref(n))
        assert rc == IFD_SUCCESS, f"power up rc={rc}"
        return bytes(atr[: n.value])

    def set_t1(self) -> None:
        rc = self.lib.IFDHSetProtocolParameters(0, SCARD_PROTOCOL_T1, 0, 0, 0, 0)
        assert rc == IFD_SUCCESS, f"set T=1 rc={rc}"

    def transmit(self, apdu: bytes) -> bytes:
        # ctypes ABI notes (bench-proven the hard way): SendPci is passed
        # BY VALUE (argtype = the struct class; a POINTER argtype shifts
        # every following register arg), and TxBuffer must be an explicit
        # create_string_buffer — cast(bytes) survives pure-python readback
        # but arrives corrupted at the libffi boundary.
        tx = ctypes.create_string_buffer(bytes(apdu), len(apdu))
        rx = (ctypes.c_ubyte * MAX_CCID_MESSAGE)()
        rn = DWORD(MAX_CCID_MESSAGE)
        send_pci = SCARD_IO_HEADER(SCARD_PROTOCOL_T1, 0)
        recv_pci = SCARD_IO_HEADER(0, 0)
        rc = self.lib.IFDHTransmitToICC(
            0, send_pci, ctypes.cast(tx, U8P), len(apdu), rx, ctypes.byref(rn),
            ctypes.byref(recv_pci))
        assert rc == IFD_SUCCESS, f"transmit rc={rc}"
        return bytes(rx[: rn.value])

    def control(self, code: int, tx: bytes) -> bytes:
        txp = ctypes.cast(
            ctypes.create_string_buffer(bytes(tx), max(1, len(tx))), U8P)
        rx = (ctypes.c_ubyte * MAX_CCID_MESSAGE)()
        n = DWORD(0)
        rc = self.lib.IFDHControl(0, code, txp, len(tx), rx, MAX_CCID_MESSAGE,
                                  ctypes.byref(n))
        assert rc == IFD_SUCCESS, f"control(0x{code:x}) rc={rc}"
        return bytes(rx[: n.value])

    def presence(self) -> int:
        return self.lib.IFDHICCPresence(0)

    def close(self):
        self.lib.IFDHCloseChannel(0)
        self.board.stop()


@pytest.fixture()
def session(request, driver):
    s = Session(driver, request.param)
    yield s
    s.close()


@pytest.mark.parametrize("session", ["satocash-g05"], indirect=True)
def test_satocash_zero_data_chain(session):
    """#170 card behavior + #169 containment through the whole driver.

    The 6-byte zero-data form gets the card's own 6700, and the session
    stays usable afterwards — the bench wedge (phantom-ATR garbage) was
    the relay's bug, and this locks the emulated card's half of the
    contract at the full IFD-driver level.
    """
    assert session.presence() == IFD_ICC_PRESENT
    assert session.power_up_atr() == G05_ATR
    session.set_t1()
    assert session.transmit(CASE2_APDU) == b"\x90\x00"
    assert session.transmit(ZERO_DATA_APDU) == b"\x67\x00"
    assert session.transmit(CASE2_APDU) == b"\x90\x00", (
        "session must survive the zero-data 6700 (#169 containment)")
    assert session.transmit(bytes([0x00, 0xA4, 0x04, 0x00, 0x00])) == b"\x90\x00"


@pytest.mark.parametrize("session", ["iso-case2"], indirect=True)
def test_iso_case2_profile_chain(session):
    assert session.presence() == IFD_ICC_PRESENT
    assert session.power_up_atr() == EID_ATR
    session.set_t1()
    assert session.transmit(bytes([0x00, 0xA4, 0x04, 0x00, 0x00])) == \
        b"\x6f\x00\x90\x00"
    rb = session.transmit(bytes([0x00, 0xB0, 0x00, 0x00, 0x10]))
    assert rb == bytes(range(0x10)) + b"\x90\x00"
    assert session.transmit(bytes([0x00, 0xB0, 0x00, 0x04, 0x04])) == \
        b"\x04\x05\x06\x07\x90\x00"
    assert session.transmit(bytes([0x00, 0x2A, 0x00, 0x00, 0x00])) == b"\x6d\x00"


@pytest.mark.parametrize("session", ["satocash-g05"], indirect=True)
def test_escape_passthrough_chain(session):
    feats = session.control(CM_IOCTL_GET_FEATURE_REQUEST, b"")
    esc_ioctl = None
    for i in range(0, len(feats) - 5, 6):
        if feats[i] == FEATURE_CCID_ESC_COMMAND:
            esc_ioctl = int.from_bytes(feats[i + 2:i + 6], "big")
    assert esc_ioctl is not None, f"features TLV missing escape: {feats.hex()}"
    ident = session.control(esc_ioctl, b"\x02")
    assert ident == b"Nucula Mock 1.0\x00"


def test_board_death_and_recovery(driver):
    """Wire-level twin of the pcscd smoke absent/recover phases."""
    s = Session(driver, "satocash-g05")
    try:
        assert s.presence() == IFD_ICC_PRESENT
        s.board.stop()
        assert s.presence() == IFD_ICC_NOT_PRESENT, (
            "dead board must map to card-absent (vpcd eject precedent)")
        revived = Board("satocash-g05", port=s.board.port)
        try:
            assert s.presence() == IFD_ICC_PRESENT, (
                "presence poll must reconnect the returned board")
            assert s.transmit(CASE2_APDU) == b"\x90\x00"
        finally:
            revived.stop()
    finally:
        s.close()


# ---- profile unit tests (no driver build needed) ----

sys.path.insert(0, str(IFD_DIR))
import card_profiles  # noqa: E402
import mock_board  # noqa: E402


def test_satocash_rule_order_beats_prefix_trap():
    card = card_profiles.SATOCASH_G05
    assert card.execute(card_profiles.ZERO_DATA_APDU) == b"\x67\x00"
    assert card.execute(card_profiles.CASE2_APDU) == b"\x90\x00"
    assert card.atr() == G05_ATR


def test_iso_case2_read_binary_determinism():
    card = card_profiles.ISO_CASE2
    assert card.execute(bytes([0x00, 0xB0, 0x00, 0x00, 0x00])) == \
        bytes(range(256)) + b"\x90\x00"
    assert card.execute(bytes([0x00, 0xA4, 0x04, 0x00, 0x02, 0xAA, 0xBB])) == \
        b"\x6f\x00\x90\x00"


def test_iso_case2_truncated_read_binary_answers_6700():
    """Codex P2 on #260: short 00 B0 prefixes must never IndexError out
    of the serve loop — they answer the card's length check 67 00."""
    card = card_profiles.ISO_CASE2
    for trunc in (b"\x00\xb0", b"\x00\xb0\x00", b"\x00\xb0\x00\x00"):
        assert card.execute(trunc) == b"\x67\x00"


def test_get_card_unknown_name_fails_loud():
    with pytest.raises(SystemExit):
        card_profiles.get_card("no-such-card")


def test_driver_rebuilds_when_source_newer(tmp_path):
    """Codex P2 on #260: a source-newer-than-binary tree must rebuild —
    the harness must never load a stale driver against current C. A
    missing toolchain skips (optional dependency); with the toolchain
    present, any make failure is a code regression and must FAIL
    (Codex P2 on #297)."""
    if not _pcsc_prereq_ok(tmp_path):
        pytest.skip(f"{CC}/PCSC headers unavailable (optional toolchain)")
    src = IFD_DIR / "ifd-nucula.c"
    os.utime(src)
    build = _build_driver()
    assert build.returncode == 0, build.stderr[-400:]
    assert SO_PATH.stat().st_mtime >= src.stat().st_mtime


def test_mock_board_default_is_legacy():
    """Back-compat lock: default construction = G-05 ATR + 90 00 for all."""
    board = mock_board.MockBoard()
    assert board.card.name == "legacy"

    def xfr(payload):
        head = bytes([mock_board.PC_XFR_BLOCK, len(payload), 0, 0, 0, 0,
                      0x01, 0, 0, 0])
        resp = board.handle(head, payload)
        plen = int.from_bytes(resp[3:7], "little")
        return resp[12:12 + plen]

    assert xfr(b"\x00\xa4\x04\x00\x00") == b"\x90\x00"
    assert xfr(b"\xff\xff") == b"\x90\x00"

    head = bytes([mock_board.PC_POWER_ON, 0, 0, 0, 0, 0, 0x02, 0, 0, 0])
    resp = board.handle(head, b"")
    plen = int.from_bytes(resp[3:7], "little")
    assert resp[12:12 + plen] == G05_ATR
