#!/usr/bin/env python3
"""Offline tests for specter_sim_relay: uscard-stub protocol + policy.

Simulates the specter-diy unix wallet side (raw APDU over TCP, one
recv) against the relay with canned backends — no pcscd, no hardware,
safe for the CI collect gate.
"""
import os
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
RELAY = REPO / "host-tools" / "specter_sim_relay.py"

sys.path.insert(0, str(REPO / "host-tools"))
import hce_relay  # noqa: E402
import specter_sim_relay  # noqa: E402


def start_relay(backend, port, journal=None):
    relay = specter_sim_relay.SimRelay(backend, journal)
    t = threading.Thread(target=relay.serve_forever, args=(port,),
                         daemon=True)
    t.start()
    for _ in range(50):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=1).close()
            return relay
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("relay did not come up")


class FakeWallet:
    """The uscard.py stub's exact behavior: send raw, one recv."""

    def __init__(self, port):
        self.s = socket.create_connection(("127.0.0.1", port), timeout=5)

    def transmit(self, apdu: bytes) -> bytes:
        self.s.sendall(apdu)
        return self.s.recv(300)

    def close(self):
        self.s.close()


def test_round_trip_scripted_card():
    # the wallet's MemoryCard SELECT (specter src/keystore/javacard/
    # applets/memorycard.py): AID B0 0B 51 11 CB 01
    table = hce_relay.parse_script([
        "00A4040006B00B5111CB01=9000",
        "0000000100=01029000",
    ])
    port = 21211
    start_relay(hce_relay.ScriptBackend(table), port)
    w = FakeWallet(port)
    try:
        assert w.transmit(bytes.fromhex("00A4040006B00B5111CB01")) == \
            bytes.fromhex("9000")
        assert w.transmit(bytes.fromhex("0000000100")) == \
            bytes.fromhex("01029000")
    finally:
        w.close()


def test_miss_answers_6a82_like_a_real_card():
    # ScriptBackend's miss default is 6A82 — the ISO file-not-found a
    # real card answers for an unknown SELECT (StaticBackend always
    # answers its fixed bytes; that is its contract).
    start_relay(hce_relay.ScriptBackend({}), 21212)
    w = FakeWallet(21212)
    try:
        assert w.transmit(bytes.fromhex("00A4040002FFFF")) == \
            bytes.fromhex("6a82")
    finally:
        w.close()


def test_policy_denial_answers_6f00_never_touches_backend():
    calls = []

    class Recording(hce_relay.CardBackend):
        name = "recording"

        def connect(self):
            calls.append("connect")

        def disconnect(self):
            calls.append("disconnect")

        def exchange(self, capdu):
            calls.append(("exchange", bytes(capdu).hex()))
            return b"\x90\x00"

        def status(self):
            return {"name": self.name}

    pol = hce_relay.JcopReadOnlyPolicy(expected_id=bytes.fromhex("50972320"))
    # pre-verified so the policy is live but armed
    pol.verified = True
    # Mirror of PcscBackend's gate in front of a recording backend:
    # verify the RELAY answers 6F00 and the backend never sees the
    # forbidden APDU.
    guarded = _GuardedBackend(Recording(), pol)
    start_relay(guarded, 21213)
    w = FakeWallet(21213)
    try:
        resp = w.transmit(bytes.fromhex("00D6000000"))  # forbidden write
        assert resp == bytes.fromhex("6f00")
    finally:
        w.close()
    assert ("exchange", "00d6000000") not in [
        c for c in calls if isinstance(c, tuple)]
    assert "connect" in calls


class _GuardedBackend(hce_relay.CardBackend):
    """Policy gate in front of another backend (the relay's PcscBackend
    does this natively; this mirrors it for offline testing)."""

    name = "guarded"

    def __init__(self, inner, policy):
        self.inner = inner
        self.policy = policy

    def connect(self):
        self.inner.connect()

    def disconnect(self):
        self.inner.disconnect()

    def exchange(self, capdu):
        self.policy.check(capdu)
        resp = self.inner.exchange(capdu)
        self.policy.observe(capdu, resp)
        return resp

    def status(self):
        return self.inner.status()


def test_cli_help_smoke():
    r = subprocess.run([sys.executable, str(RELAY), "--help"],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0 and "specter" in r.stdout


SPECTER_DIR = Path(os.environ.get(
    "SPECTER_DYI_TREE",
    "/root/src/specter-diy" if os.geteuid() == 0 else str(
        Path.home() / "src" / "specter-diy")))


def _specter_dir_exists():
    # Path.stat on /root/* raises PermissionError (not False) for
    # non-root CI runners — .exists() propagates it at module level
    # and kills the unprivileged python-collect gate.
    try:
        return SPECTER_DIR.exists()
    except (PermissionError, OSError):
        return False
def _load_specter_applet():
    """Import the wallet's MemoryCardApplet standalone under CPython.

    The wallet targets MicroPython; three stdlib-module shims
    (micropython.const, rng, ucryptolib) and a synthetic package
    skeleton are enough for the applet layer — the uscard stub itself
    runs unmodified.
    """
    import types as T
    import importlib
    if "micropython" not in sys.modules:
        mp = T.ModuleType("micropython")
        mp.const = lambda x: x
        sys.modules["micropython"] = mp
    if "rng" not in sys.modules:
        r = T.ModuleType("rng")
        r.get_random_bytes = lambda n: __import__("os").urandom(n)
        sys.modules["rng"] = r
    if "ucryptolib" not in sys.modules:
        uc = T.ModuleType("ucryptolib")

        class _AES:
            def __init__(self, key, mode, iv=None):
                from Crypto.Cipher import AES as _A
                self._c = _A.new(
                    key, _A.MODE_ECB if mode == 0 else _A.MODE_CBC, iv=iv)

            def encrypt(self, d):
                return self._c.encrypt(d)

            def decrypt(self, d):
                return self._c.decrypt(d)
        uc.aes = _AES
        uc.MODE_ECB = 0
        uc.MODE_CBC = 1
        sys.modules["ucryptolib"] = uc
    sys.path.insert(0, str(SPECTER_DIR / "f469-disco" / "libs" / "unix"))
    jc = SPECTER_DIR / "src" / "keystore" / "javacard"

    def load(name, path):
        spec = importlib.util.spec_from_file_location(name, path)
        m = importlib.util.module_from_spec(spec)
        sys.modules[name] = m
        spec.loader.exec_module(m)
        return m

    pkg = T.ModuleType("jk")
    pkg.__path__ = [str(jc)]
    sys.modules["jk"] = pkg
    ap = T.ModuleType("jk.applets")
    ap.__path__ = [str(jc / "applets")]
    sys.modules["jk.applets"] = ap
    load("jk.util", str(jc / "util.py"))
    load("jk.applets.applet", str(jc / "applets" / "applet.py"))
    return importlib.import_module("jk.applets.memorycard")


@pytest.mark.skipif(not _specter_dir_exists(),
                    reason="specter-diy tree absent (bench host only)")
def test_e2e_specter_wallet_select_through_relay():
    """The REAL wallet applet class + REAL uscard stub -> our relay ->
    scripted card: the zero-hardware wallet<->card loop."""
    pytest.importorskip("Crypto")
    mc = _load_specter_applet()
    from uscard import Reader
    table = hce_relay.parse_script(["00A4040006B00B5111CB01=9000"])
    start_relay(hce_relay.ScriptBackend(table), 21214)
    conn = Reader().createConnection()
    assert conn.isCardInserted()
    conn.connect(1)
    mc.MemoryCardApplet(conn).select()  # raises ISOException on != 9000
