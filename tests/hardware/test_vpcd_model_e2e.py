#!/usr/bin/env python3
"""E2E: the Specter MemoryCard model hosted behind a REAL pcscd via vpcd.

    pyscard -> private pcscd -> libifdvpcd (vpcd TCP) -> vpcd_card_server
            -> MemoryCardModel

Entirely dry: no bench hardware, no board — and no touch of the FROZEN
system pcscd or any lane reader config (AGENTS.md "Host pcscd
instances" freeze). A dedicated pcscd instance is spawned on a private
unix socket with a private reader.conf (own high port, far from every
lane slot), and torn down completely in ``finally`` (process killed and
reaped, socket file removed, no vpcd slot touched).

Collection safety (#403 follow-up — same rules as test_vjavacard.py and
test_specter_card_model.py): only stdlib + the stdlib-only
vpcd_card_server import at module scope; every heavy import is guarded
(importorskip BEFORE the model import) and every existence / port
probe happens INSIDE the tests, so the CI collect gate stays green on
runners that have none of pyscard / vpcd / coincurve.

Local run (whole invocation wrapped in timeout — a wedged accept would
otherwise hang CI):

    timeout 300 /usr/bin/python3 -m pytest \\
        tests/hardware/test_vpcd_model_e2e.py -v

Requirements: pcscd + a libifdvpcd (distro stock works; the
Amperstrand-patched build is preferred when present — the stock
driver's first absent-probe kills its event thread, #349 class; this
test starts the vpicc BEFORE pcscd so the driver's first probe already
finds a card), systemd-socket-activate, pyscard, coincurve,
pycryptodome.
"""
from __future__ import annotations

import os
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "host-tools"))

# stdlib-only — safe to import on any runner, keeps the framing tests
# runnable everywhere (they need no pcscd at all)
import vpcd_card_server  # noqa: E402

READER_NAME = "model-vpcd"          # FRIENDLYNAME in the private reader.conf
DEFAULT_PORT = 28350                # own high port, no lane uses this range
MEMORYCARD_AID = bytes.fromhex("B00B5111CB01")
GET_PUBKEY = [0xB0, 0xB2, 0x00, 0x00]

#: vpcd driver candidates, most-preferred first. The Amperstrand patch
#: (tests/hardware/host/libifdvpcd-amperstrand.patch) fixes the
#: absent-probe event-thread death and the un-timeouted recvall — but
#: any build speaks the same wire protocol.
VPCD_LIB_CANDIDATES = (
    "/usr/local/lib/pcsc/drivers/serial/libifdvpcd-amperstrand.so",
    "/usr/lib/pcsc/drivers/vpcd.bundle/Contents/Linux/lib_vpcd.so",
    "/usr/lib/pcsc/drivers/serial/libifdvpcd.so",
)
PCSCD_BIN = "/usr/sbin/pcscd"


def _listening_ports() -> set[int]:
    """Ports in LISTEN state from /proc/net/tcp[6] — the ONLY non-
    destructive way to check a vpcd slot (a TCP probe would displace a
    live bridge's backlog-1 reconnect; bench lesson 2026-11-14)."""
    ports: set[int] = set()
    for procfile in ("/proc/net/tcp", "/proc/net/tcp6"):
        try:
            lines = open(procfile, encoding="ascii").read().splitlines()[1:]
        except OSError:
            continue
        for line in lines:
            parts = line.split()
            if len(parts) < 4:
                continue
            if parts[3] == "0A":  # LISTEN
                ports.add(int(parts[1].split(":")[1], 16))
    return ports


def _pick_port() -> int | None:
    """DEFAULT_PORT (or $VPCD_MODEL_PORT) if free, else scan upward."""
    start = int(os.environ.get("VPCD_MODEL_PORT", DEFAULT_PORT))
    busy = _listening_ports()
    for port in range(start, start + 32):
        if port not in busy:
            return port
    return None


def _find_vpcd_lib() -> str | None:
    override = os.environ.get("VPCD_MODEL_LIBPATH")
    if override:
        return override if Path(override).exists() else None
    for cand in VPCD_LIB_CANDIDATES:
        if Path(cand).exists():
            return cand
    return None


# ---------------------------------------------------------------------------
# Offline protocol tests — no pcscd, no vpcd: the session loop over a
# socketpair. These run on ANY runner (stdlib only).
# ---------------------------------------------------------------------------


class _EchoBackend:
    """Minimal CardBackend: echoes the C-APDU + SW 9000, records events."""

    def __init__(self, atr: bytes = b"\x3b\x00"):
        self.atr = atr
        self.events: list = []

    def connect(self):
        self.events.append("connect")

    def disconnect(self):
        self.events.append("disconnect")

    def exchange(self, capdu: bytes) -> bytes:
        self.events.append(bytes(capdu))
        return bytes(capdu) + b"\x90\x00"


class _ExplodingBackend:
    atr = b"\x3b\x00"

    def exchange(self, capdu: bytes) -> bytes:
        raise RuntimeError("backend blew up")


def _recv_frame(sock: socket.socket, timeout: float = 2.0) -> bytes:
    """Read one length-prefixed message (raises socket.timeout on none)."""
    sock.settimeout(timeout)

    def exact(n: int) -> bytes:
        buf = b""
        while len(buf) < n:
            chunk = sock.recv(n - len(buf))
            if not chunk:
                raise AssertionError("peer closed mid-frame")
            buf += chunk
        return buf

    (length,) = struct.unpack(">H", exact(2))
    return exact(length) if length else b""


@pytest.fixture()
def session_pair():
    """A serve() loop over a socketpair + the driving end of it."""
    backend = _EchoBackend()
    server = vpcd_card_server.VpcdCardServer(backend)
    ours, theirs = socket.socketpair()
    thread = threading.Thread(target=server.serve, args=(theirs,),
                              daemon=True)
    thread.start()
    yield server, backend, ours
    server.stop()
    ours.close()
    theirs.close()
    thread.join(timeout=5)


class TestVpcdProtocolOffline:
    def test_get_atr_answers_backend_atr(self, session_pair):
        server, _, ours = session_pair
        ours.sendall(b"\x00\x01\x04")           # len 1, CTRL_GET_ATR
        assert _recv_frame(ours) == b"\x3b\x00"

    def test_power_and_reset_controls_are_silent(self, session_pair):
        """OFF/ON/RESET get NO reply — the next frame after all three
        must be the GetATR answer (an empty ack desyncs the driver)."""
        server, backend, ours = session_pair
        ours.sendall(b"\x00\x01\x00")           # CTRL_OFF
        ours.sendall(b"\x00\x01\x01")           # CTRL_ON
        ours.sendall(b"\x00\x01\x02")           # CTRL_RESET
        ours.sendall(b"\x00\x01\x04")           # CTRL_GET_ATR
        assert _recv_frame(ours) == b"\x3b\x00"
        assert backend.events[:3] == ["disconnect", "connect",
                                      "disconnect"]
        # RESET = disconnect + connect (warm reset keeps transport alive)
        assert backend.events[3] == "connect"

    def test_apdu_relayed_to_backend_with_prefix(self, session_pair):
        server, backend, ours = session_pair
        capdu = bytes([0x00, 0xA4, 0x04, 0x00, 0x06]) + MEMORYCARD_AID
        ours.sendall(struct.pack(">H", len(capdu)) + capdu)
        assert _recv_frame(ours) == capdu + b"\x90\x00"
        assert backend.events[-1] == capdu

    def test_long_reply_uses_two_byte_prefix(self, session_pair):
        server, backend, ours = session_pair
        capdu = b"\x80\xAA\x00\x00" + b"\x11" * 300
        ours.sendall(struct.pack(">H", len(capdu)) + capdu)
        reply = _recv_frame(ours)
        assert reply == capdu + b"\x90\x00"    # 306 bytes: 0x0132 prefix

    def test_backend_exception_answers_6f00(self, session_pair):
        server, _, ours = session_pair
        server.backend = _ExplodingBackend()
        ours.sendall(b"\x00\x02\x80\x00")
        assert _recv_frame(ours) == b"\x6f\x00"

    def test_backend_short_answer_answers_6f00(self, session_pair):
        server, _, ours = session_pair

        class Short(_EchoBackend):
            def exchange(self, capdu):
                return b"\x90"  # < 2 bytes: not a status word

        server.backend = Short()
        ours.sendall(b"\x00\x02\x80\x00")
        assert _recv_frame(ours) == b"\x6f\x00"

    def test_eof_ends_session_cleanly(self, session_pair):
        server, _, ours = session_pair
        ours.close()
        # the serve() thread returns; the fixture's join must not hang


def test_model_exposes_comsign_atr():
    pytest.importorskip("Crypto")
    pytest.importorskip("coincurve")
    import specter_card_model  # noqa: E402 — after the guards

    model = specter_card_model.MemoryCardModel()
    assert model.atr == specter_card_model.COMSIGN_EID_ATR
    assert model.atr.hex().upper() == "3BD518FF8191FE1FC38073C821100A"
    # sane ATR shape: TS=0x3B direct convention, TCK present (even parity
    # over everything after T0), a T=1 protocol indicator on the wire
    assert model.atr[0] == 0x3B
    assert model.atr[1] & 0x0F == 5  # 5 historical bytes
    checksum = 0
    for byte in model.atr[1:]:
        checksum ^= byte
    assert checksum == 0


# ---------------------------------------------------------------------------
# The real thing: private pcscd + vpcd + the model, driven with pyscard.
# ---------------------------------------------------------------------------


def test_memorycard_model_e2e_through_real_pcscd(tmp_path):
    libpath = _find_vpcd_lib()
    if libpath is None:
        pytest.skip("no libifdvpcd/vpcd bundle on this host")
    if not Path(PCSCD_BIN).exists():
        pytest.skip(f"{PCSCD_BIN} not installed")
    if shutil.which("systemd-socket-activate") is None:
        pytest.skip("systemd-socket-activate not available")
    pytest.importorskip("smartcard",
                        reason="pyscard not installed (bench host only)")
    pytest.importorskip("Crypto")
    pytest.importorskip("coincurve")
    import specter_card_model  # noqa: E402 — after the guards
    from smartcard.Exceptions import CardConnectionException
    from smartcard.System import readers

    port = _pick_port()
    if port is None:
        pytest.skip("no free high port for a private vpcd slot")
    assert port not in _listening_ports(), "picked a busy port?!"

    conf = tmp_path / "reader.conf"
    # DEVICENAME hostname part must be literally /dev/null: ifd-vpcd's
    # IFDHCreateChannelByName treats anything else as a remote vpicc
    # HOSTNAME to connect OUT to (upstream pin 93286a90) — the
    # listen-mode form the bench lanes use is /dev/null:<port>.
    conf.write_text(
        f'FRIENDLYNAME "{READER_NAME}"\n'
        f"DEVICENAME /dev/null:{port}\n"
        f"LIBPATH {libpath}\n"
        f"CHANNELID 1\n"
    )
    sockpath = tmp_path / "pcscd.sock"

    backend = specter_card_model.MemoryCardModel()
    server = vpcd_card_server.VpcdCardServer(backend, host="127.0.0.1",
                                             port=port)
    # vpicc FIRST: it retries the connect until the driver listens, so
    # pcscd's very first slot probe already finds the card inserted
    # (stock-driver absent-probe hardening, #349 class)
    server.start()

    env = dict(os.environ, PCSCLITE_CSOCK_NAME=str(sockpath))
    pcscd_log = open(tmp_path / "pcscd.log", "wb")  # noqa: SIM115
    proc = subprocess.Popen(
        ["systemd-socket-activate", "-l", str(sockpath), "--",
         PCSCD_BIN, "--foreground", "-c", str(conf)],
        env=env, stdout=pcscd_log, stderr=subprocess.STDOUT)

    old_csock = os.environ.get("PCSCLITE_CSOCK_NAME")
    os.environ["PCSCLITE_CSOCK_NAME"] = str(sockpath)
    try:
        # systemd-socket-activate spawns pcscd lazily on the FIRST client
        # connection — every readers() poke below starts/keeps pcscd up,
        # the driver opens the vpcd listener, and the vpicc's retry loop
        # attaches. Wait for the whole chain, not for any single link.
        rs = []
        deadline = time.time() + 30
        while time.time() < deadline:
            if not server.wait_attached(0) or not rs:
                try:
                    rs = [r for r in readers() if READER_NAME in str(r)]
                except Exception:
                    pass  # pcscd still waking up behind socket activation
            if rs and server.wait_attached(0):
                break
            time.sleep(0.25)
        assert rs, (
            f"{READER_NAME} reader never appeared "
            f"(pcscd rc={proc.poll()}, log tail: "
            f"{(tmp_path / 'pcscd.log').read_text(errors='replace')[-500:]}")
        assert server.wait_attached(0), (
            "reader appeared but the card-side connection never "
            "attached — reader.conf DEVICENAME port wrong?")
        reader_name = str(rs[0])
        print(f"reader enumerated: {reader_name}")

        conn = None
        deadline = time.time() + 15
        while time.time() < deadline:
            try:
                conn = rs[0].createConnection()
                conn.connect()
                break
            except (CardConnectionException, Exception):
                conn = None
                time.sleep(0.4)
        assert conn is not None, "card never became present in the slot"

        try:
            atr = bytes(conn.getATR())
            print(f"ATR: {atr.hex().upper()}")
            assert atr == backend.atr

            def transmit(apdu):
                """One transmit with bounded retry (the freshly-started
                vpcd slot can answer empty for a moment — dead-window
                class; do not fail-fast on a young pcscd)."""
                deadline = time.time() + 15
                while True:
                    try:
                        return conn.transmit(apdu)
                    except (CardConnectionException, Exception):
                        if time.time() >= deadline:
                            raise
                        time.sleep(0.4)

            # SELECT the MemoryCard applet
            sel = [0x00, 0xA4, 0x04, 0x00, len(MEMORYCARD_AID)]
            data, sw1, sw2 = transmit(sel + list(MEMORYCARD_AID))
            print(f"SELECT -> SW {sw1:02X}{sw2:02X} data={bytes(data).hex()}")
            assert (sw1, sw2) == (0x90, 0x00)

            # GET_PUBKEY: 65-byte uncompressed secp256k1 point + 9000
            data, sw1, sw2 = transmit(GET_PUBKEY)
            print(f"GET_PUBKEY -> SW {sw1:02X}{sw2:02X} "
                  f"len={len(data)} head={bytes(data[:2]).hex()}")
            assert (sw1, sw2) == (0x90, 0x00)
            assert len(data) == 65
            assert data[0] == 0x04
        finally:
            if conn is not None:
                conn.disconnect()
        # let pyscard's PCSCCardConnection finalizers run while OUR
        # pcscd is still up (GC after teardown surfaces a benign
        # "Service not available" unraisable from __del__)
        conn = None
        rs = []
        import gc
        gc.collect()
    finally:
        server.stop()
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
        pcscd_log.close()
        # systemd-socket-activate creates the socket FILE and leaves it
        # behind on exit — it is our file, we remove it
        try:
            sockpath.unlink()
        except FileNotFoundError:
            pass
        if old_csock is None:
            os.environ.pop("PCSCLITE_CSOCK_NAME", None)
        else:
            os.environ["PCSCLITE_CSOCK_NAME"] = old_csock

    # teardown proven: process reaped, socket gone, slot port released
    assert proc.poll() is not None, "private pcscd still running"
    assert not sockpath.exists(), "private pcscd socket file left behind"
    assert port not in _listening_ports(), "vpcd slot port still LISTENing"
