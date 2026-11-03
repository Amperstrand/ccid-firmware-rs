#!/usr/bin/env python3
"""Phone-B ReaderMode backend tests: protocol, session, safety boundary.

The AndroidReaderBackend speaks the REVERSED direction of the HCE link:
the HOST sends C-APDUs down (adb forward tcp:4872 ->
localabstract:bench-reader-relay) and the phone's ReaderMode app
answers with the physical card's R-APDUs (or controls: 0x01 tag-lost,
0x03 tag-present). These tests simulate the phone with a socketpair
peer and a FakeTransport — no phone, no adb.

Run: python3 -m pytest tests/hardware/test_hce_relay_android.py -v
"""
from __future__ import annotations

import socket
import sys
import threading
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "host-tools"))

import hce_relay  # noqa: E402


def make_phone():
    """socketpair peer playing the phone's RelayReaderServer."""
    phone, host = socket.socketpair()
    phone.settimeout(5)
    return phone, host


def start_phone_server(phone, responder):
    """Serve host->phone C-APDU frames with responder(capdu)->rapdu."""
    def serve():
        try:
            while True:
                capdu = hce_relay.recv_frame(phone)
                rapdu = responder(capdu)
                hce_relay.send_frame(phone, rapdu)
        except (EOFError, OSError):
            pass
    t = threading.Thread(target=serve, daemon=True)
    t.start()
    return t


def connected_backend(policy=None):
    phone, host = make_phone()
    be = hce_relay.AndroidReaderBackend(policy=policy, unsafe_raw=policy is None)
    be._sock = host
    be._connected = True
    return be, phone, host


GV_P1 = bytes.fromhex("04040230001105") + b"\x91\xaf"
GV_P3 = bytes.fromhex("04c474fa967380cf2e5649501921") + b"\x91\x00"


# --- protocol ---------------------------------------------------------------


def test_exchange_round_trip():
    be, phone, host = connected_backend()
    t = start_phone_server(phone, lambda c: b"\x90\x00")
    try:
        assert be.exchange(bytes.fromhex("00A4040000")) == b"\x90\x00"
    finally:
        phone.close()
        host.close()
        t.join(timeout=2)


def test_tag_lost_is_explicit_backend_error():
    be, phone, host = connected_backend()

    def serve():
        hce_relay.recv_frame(phone)
        hce_relay.send_frame(phone, b"\x01")   # tag-lost control
    threading.Thread(target=serve, daemon=True).start()
    try:
        with pytest.raises(hce_relay.BackendError):
            be.exchange(bytes.fromhex("00A4040000"))
    finally:
        phone.close()
        host.close()


def test_construction_guard_matches_pcsc():
    with pytest.raises(hce_relay.RelayError):
        hce_relay.AndroidReaderBackend()
    be = hce_relay.AndroidReaderBackend(unsafe_raw=True)
    assert be.status()["policy"] == "raw-unsafe"


# --- safety boundary applies on the android path too -------------------------


def policy_backend():
    pol = hce_relay.Ntag424ReadOnlyPolicy(
        expected_id=bytes.fromhex("04c474fa967380"))
    be, phone, host = connected_backend(policy=pol)
    return be, phone, host, pol


def test_forbidden_write_never_sent_down_the_socket():
    be, phone, host, pol = policy_backend()
    sent = []

    def serve():
        try:
            while True:
                sent.append(hce_relay.recv_frame(phone))
                hce_relay.send_frame(phone, b"\x91\x00")
        except (EOFError, OSError):
            pass
    threading.Thread(target=serve, daemon=True).start()
    try:
        with pytest.raises(hce_relay.PolicyViolation):
            be.exchange(bytes.fromhex("00D6000000"))
        assert sent == []   # nothing reached the phone, let alone the card
    finally:
        phone.close()
        host.close()


def test_identity_verification_through_the_phone():
    be, phone, host, pol = policy_backend()
    answers = iter([GV_P1, GV_P1, GV_P3])

    def serve():
        try:
            while True:
                hce_relay.recv_frame(phone)
                hce_relay.send_frame(phone, next(answers, b"\x91\x00"))
        except (EOFError, OSError, StopIteration):
            pass
    threading.Thread(target=serve, daemon=True).start()
    try:
        be.exchange(bytes.fromhex("9060000000"))
        be.exchange(bytes.fromhex("90AF000000"))
        be.exchange(bytes.fromhex("90AF000000"))
        assert pol.verified
        assert be.exchange(bytes.fromhex("00B0000002")) == b"\x91\x00"
    finally:
        phone.close()
        host.close()


# --- FakeReaderBackend: the offline Phone-B stand-in --------------------------


def test_fake_reader_backend_serves_canned_tag():
    tag = {bytes.fromhex("00A4040007D276000085010100"): b"\x90\x00"}
    be = hce_relay.FakeReaderBackend(tag, unsafe_raw=True)
    be.connect()
    assert be.exchange(bytes.fromhex("00A4040007D276000085010100")) == \
        b"\x90\x00"
    be.disconnect()


def test_fake_reader_backend_tag_lost_injection():
    be = hce_relay.FakeReaderBackend({}, lose_after=0, unsafe_raw=True)
    be.connect()
    with pytest.raises(hce_relay.BackendError):
        be.exchange(bytes.fromhex("00A4040000"))


def test_fake_reader_backend_policy_guard():
    with pytest.raises(hce_relay.RelayError):
        hce_relay.FakeReaderBackend({})
