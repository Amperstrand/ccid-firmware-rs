#!/usr/bin/env python3
"""hce_relay.py host-only tests: framing, script tables, backends, session.

The relay brain (host-tools/hce_relay.py, #529/#529-kickoff) must be
testable with stdlib only — no pyscard, no adb, no phone. These tests
drive the framing/session layers with socket.socketpair() playing the
phone (RelayServer.java side) and in-process CardBackend stubs.

Run: python3 -m pytest tests/hardware/test_hce_relay.py -v
"""
from __future__ import annotations

import json
import socket
import sys
import threading
import time
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
HOST_TOOLS = REPO_ROOT / "host-tools"
sys.path.insert(0, str(HOST_TOOLS))

import hce_relay  # noqa: E402

# --- framing ---------------------------------------------------------------


def test_framing_roundtrip():
    a, b = socket.socketpair()
    try:
        hce_relay.send_frame(a, bytes.fromhex("00A40400"))
        assert hce_relay.recv_frame(b) == bytes.fromhex("00A40400")
    finally:
        a.close()
        b.close()


def test_frame_spanning_segment_writes():
    """recv_frame must survive arbitrary segmentation (the adb forward path)."""
    a, b = socket.socketpair()
    payload = bytes(range(64))
    try:
        wire = len(payload).to_bytes(2, "big") + payload
        for byte in wire:
            a.send(bytes([byte]))
        assert hce_relay.recv_frame(b) == payload
    finally:
        a.close()
        b.close()


def test_recv_frame_eof_mid_header_is_clean():
    a, b = socket.socketpair()
    try:
        a.send(b"\x00")  # half a length header
        a.close()
        with pytest.raises((EOFError, OSError)):
            hce_relay.recv_frame(b)
    finally:
        b.close()


def test_send_frame_rejects_oversize():
    a, b = socket.socketpair()
    try:
        with pytest.raises(hce_relay.RelayError):
            hce_relay.send_frame(a, b"x" * (hce_relay.MAX_FRAME + 1))
    finally:
        a.close()
        b.close()


# --- script tables ----------------------------------------------------------


def test_parse_script_basic_and_comments():
    lines = [
        "# a comment",
        "00A4040007D276000085010100 = 9000",
        "",
        "00CA004F00=6A88   # trailing comment",
    ]
    table = hce_relay.parse_script(lines)
    assert table == {
        bytes.fromhex("00A4040007D276000085010100"): bytes.fromhex("9000"),
        bytes.fromhex("00CA004F00"): bytes.fromhex("6A88"),
    }


def test_parse_script_dash_is_stdin():
    """RUNBOOK regression: `--script -` must read stdin, not a file named '-'.

    The documented invocation is
        hce_relay.py --script - <<'EOF' ...
    and it used to die with FileNotFoundError(open('-')).
    """
    table = hce_relay.load_script("-", stdin="00A4040000=9000\n80CA9F7F00=9000\n")
    assert table == {
        bytes.fromhex("00A4040000"): bytes.fromhex("9000"),
        bytes.fromhex("80CA9F7F00"): bytes.fromhex("9000"),
    }


def test_parse_script_duplicate_capdu_is_rejected():
    with pytest.raises(hce_relay.RelayError) as ei:
        hce_relay.parse_script(["00A4040000=9000", "00A4040000=6A82"])
    assert "duplicate" in str(ei.value).lower()


def test_parse_script_bad_hex_is_rejected_with_line_number():
    with pytest.raises(hce_relay.RelayError) as ei:
        hce_relay.parse_script(["00A4040000=9000", "ZZ=9000"])
    assert "line 2" in str(ei.value)


def test_parse_script_missing_separator_is_rejected():
    with pytest.raises(hce_relay.RelayError):
        hce_relay.parse_script(["00A4040000 9000"])


# --- backends ---------------------------------------------------------------


def test_static_backend_answers_fixed_response():
    be = hce_relay.StaticBackend(bytes.fromhex("9000"))
    assert be.exchange(bytes.fromhex("00A4040000")) == bytes.fromhex("9000")
    assert be.status()["name"] == "static"


def test_script_backend_hit_and_default_miss():
    be = hce_relay.ScriptBackend(
        {bytes.fromhex("00A4040000"): bytes.fromhex("9000")})
    assert be.exchange(bytes.fromhex("00A4040000")) == bytes.fromhex("9000")
    # default miss = 6A82 (file not found) — never a fabricated success SW
    assert be.exchange(bytes.fromhex("00B0950000")) == bytes.fromhex("6A82")
    assert be.status()["name"] == "script"


def test_script_backend_custom_miss():
    be = hce_relay.ScriptBackend({}, miss=bytes.fromhex("6E00"))
    assert be.exchange(bytes.fromhex("00A4040000")) == bytes.fromhex("6E00")


def test_pcsc_backend_import_is_lazy():
    """Module import must never require pyscard (python-collect env has none)."""
    assert "smartcard" not in sys.modules
    be = hce_relay.PcscBackend("needle")
    assert be.status()["name"] == "pcsc"
    assert "smartcard" not in sys.modules  # construction alone stays pure


class ExplodingBackend(hce_relay.CardBackend):
    name = "exploding"

    def __init__(self, on_nth=1):
        self.on_nth = on_nth
        self.calls = 0

    def exchange(self, capdu: bytes) -> bytes:
        self.calls += 1
        if self.calls >= self.on_nth:
            raise hce_relay.BackendError("card vanished")
        return bytes.fromhex("9000")


# --- session: one phone socket <-> one backend, strictly sequential ---------


def make_session(backend, tmp_path, **kw):
    phone, host = socket.socketpair()
    journal = hce_relay.JsonlJournal(tmp_path / "journal.jsonl")
    session = hce_relay.RelaySession(host, backend, journal=journal, **kw)
    return phone, host, session, journal


def start_session(session, **kw):
    """Run the session loop in a thread (the phone side blocks on reads)."""
    t = threading.Thread(target=session.run, kwargs=kw, daemon=True)
    t.start()
    return t


def read_frame(sock):
    return hce_relay.recv_frame(sock)


def test_session_hello_and_exchange(tmp_path):
    phone, host, session, journal = make_session(
        hce_relay.ScriptBackend(
            {bytes.fromhex("00A4040000"): bytes.fromhex("9000")}),
        tmp_path)
    phone.settimeout(5)
    try:
        t = start_session(session, once=1)
        phone.send(b"\x00\x01\x02")  # hello control
        phone.send(b"\x00\x05" + bytes.fromhex("00A4040000"))
        assert read_frame(phone) == bytes.fromhex("9000")
        t.join(timeout=5)
        assert not t.is_alive()
    finally:
        phone.close()
        host.close()
    events = [json.loads(line) for line in journal.path.read_text().splitlines()]
    kinds = [e["ev"] for e in events]
    assert "hello" in kinds
    assert "rx" in kinds and "tx" in kinds


def test_session_controls_do_not_reach_backend(tmp_path):
    class SpyBackend(hce_relay.CardBackend):
        name = "spy"
        def __init__(self):
            self.calls = 0
        def exchange(self, capdu):
            self.calls += 1
            return b"\x90\x00"

    spy = SpyBackend()
    phone, host, session, journal = make_session(spy, tmp_path)
    phone.settimeout(5)
    try:
        phone.send(b"\x00\x01\x01")  # link deactivated
        phone.send(b"\x00\x01\x02")  # hello
        phone.close()  # EOF ends the session
        assert session.run() == 0
    finally:
        host.close()
    assert spy.calls == 0
    events = [json.loads(line) for line in journal.path.read_text().splitlines()]
    assert "deactivated" in [e["ev"] for e in events]


def test_session_backend_error_is_explicit_no_reply(tmp_path):
    """Backend failure must surface as an error + closed phone socket —
    never a stale/fabricated reply paired with the failed C-APDU."""
    phone, host, session, journal = make_session(ExplodingBackend(), tmp_path)
    phone.settimeout(5)
    try:
        phone.send(b"\x00\x02\x00\x00")  # C-APDU 0000 -> backend explodes
        assert session.run() == 1
        # phone side must see EOF (no reply was ever sent)
        assert phone.recv(16) == b""
    finally:
        phone.close()
        host.close()
    events = [json.loads(line) for line in journal.path.read_text().splitlines()]
    assert "error" in [e["ev"] for e in events]


def test_session_missing_rapdu_prefix_is_rejected(tmp_path):
    """Every reply must carry a status word — a 1-byte reply cannot be
    framed onto ISO-DEP (RelayApduService substitutes 6F00, but the
    brain should refuse to be that wrong in the first place)."""
    class OneByteBackend(hce_relay.CardBackend):
        name = "onebyte"
        def exchange(self, capdu):
            return b"\x6A"
    phone, host, session, journal = make_session(OneByteBackend(), tmp_path)
    phone.settimeout(5)
    try:
        phone.send(b"\x00\x02\x00\x00")
        assert session.run() == 1
        assert phone.recv(16) == b""
    finally:
        phone.close()
        host.close()


def test_session_delay_ms_bounds_answer_latency(tmp_path):
    phone, host, session, journal = make_session(
        hce_relay.StaticBackend(b"\x90\x00"), tmp_path, delay_ms=60)
    phone.settimeout(5)
    try:
        t = start_session(session, once=1)
        t0 = time.monotonic()
        phone.send(b"\x00\x02\x00\x00")
        assert read_frame(phone) == b"\x90\x00"
        assert time.monotonic() - t0 >= 0.055
        t.join(timeout=5)
        assert not t.is_alive()
    finally:
        phone.close()
        host.close()


def test_session_disconnects_backend_on_eof(tmp_path):
    class TrackingBackend(hce_relay.CardBackend):
        name = "tracking"
        def __init__(self):
            self.connected = False
            self.disconnected = False
        def connect(self):
            self.connected = True
        def disconnect(self):
            self.disconnected = True
        def exchange(self, capdu):
            return b"\x90\x00"
    be = TrackingBackend()
    phone, host, session, journal = make_session(be, tmp_path)
    phone.settimeout(5)
    try:
        t = start_session(session, once=1)
        phone.send(b"\x00\x02\x00\x00")
        read_frame(phone)
        t.join(timeout=5)
        assert not t.is_alive()
    finally:
        phone.close()
        host.close()
    assert be.connected and be.disconnected


# --- journal + redaction ----------------------------------------------------


def test_journal_redacts_payloads_by_default(tmp_path):
    phone, host, session, journal = make_session(
        hce_relay.StaticBackend(b"\x90\x00"), tmp_path)
    journal.log_hex = False
    phone.settimeout(5)
    try:
        t = start_session(session, once=1)
        secret = bytes.fromhex("00A4040007D276000085010100")
        phone.send(len(secret).to_bytes(2, "big") + secret)
        assert read_frame(phone) == b"\x90\x00"
        t.join(timeout=5)
        assert not t.is_alive()
    finally:
        phone.close()
        host.close()
    raw = journal.path.read_text()
    assert "d2760000850101" not in raw.lower()
    events = [json.loads(line) for line in raw.splitlines()]
    rx = next(e for e in events if e["ev"] == "rx")
    assert rx["len"] == len(secret)
    assert rx["sha8"] == __import__("hashlib").sha256(secret).hexdigest()[:8]


def test_journal_optin_hex(tmp_path):
    journal = hce_relay.JsonlJournal(tmp_path / "j.jsonl", log_hex=True)
    journal.log("rx", data=bytes.fromhex("00A4040000"))
    assert "00a4040000" in journal.path.read_text().lower()
