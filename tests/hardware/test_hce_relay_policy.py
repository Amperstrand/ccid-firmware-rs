#!/usr/bin/env python3
"""Card-safety policy tests: default-deny at the physical-card boundary.

The PcscBackend must refuse to transmit anything a per-card read-only
profile does not explicitly allow, must require positive card identity
before unlocking a profile, and must treat stateful native commands
(90 AF continuation) as unclassifiable without chain context. These
tests use a recording fake transport — if a forbidden APDU ever reaches
it, the test fails.

Run: python3 -m pytest tests/hardware/test_hce_relay_policy.py -v
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "host-tools"))

import hce_relay  # noqa: E402


class FakeTransport:
    """Records C-APDUs; returns canned R-APDUs."""

    def __init__(self, responses=None):
        self.calls = []
        self.responses = list(responses or [])

    def __call__(self, capdu: bytes) -> bytes:
        self.calls.append(bytes(capdu))
        if self.responses:
            r = self.responses.pop(0)
            return r if isinstance(r, bytes) else bytes(r)
        return b"\x91\x00"


def backend(policy):
    be = hce_relay.PcscBackend.__new__(hce_relay.PcscBackend)
    be.needle = "fake"
    be._conn = object()  # nonzero => connected
    be._atr = None
    be.policy = policy
    be._transmit_impl = FakeTransport()
    return be


def allowed(be, capdu_hex):
    return be.exchange(bytes.fromhex(capdu_hex))


def denied(be, capdu_hex):
    before = len(be._transmit_impl.calls)
    with pytest.raises(hce_relay.PolicyViolation):
        be.exchange(bytes.fromhex(capdu_hex))
    assert len(be._transmit_impl.calls) == before, \
        "forbidden APDU reached the transport"


NT424_UID = "04c474fa967380"
GV_P1 = bytes.fromhex("04040230001105") + b"\x91\xaf"
GV_P2 = bytes.fromhex("04040201021105") + b"\x91\xaf"
GV_P3 = bytes.fromhex(NT424_UID + "cf2e5649501921") + b"\x91\x00"


def nt424_backend(uid=NT424_UID, responses=None):
    pol = hce_relay.Ntag424ReadOnlyPolicy(expected_id=bytes.fromhex(uid))
    be = backend(pol)
    if responses is not None:
        be._transmit_impl.responses = list(responses)
    return be, pol


# --- NTAG424: allowlist shape ------------------------------------------------


def test_nt424_get_version_chain_is_the_only_preverification_command():
    # chain stays open (91AF replies) so no identity decision fires —
    # this test exercises the GATE, not the verification.
    be, _ = nt424_backend(responses=[b"\x91\xaf", b"\x91\xaf",
                                     b"\x91\xaf", b"\x91\xaf"])
    allowed(be, "9060000000")          # opens the chain (identity gate)
    allowed(be, "90AF000000")          # continuation INSIDE the chain


def test_nt424_continuation_without_chain_is_denied():
    be, _ = nt424_backend()
    denied(be, "90AF000000")           # 90 AF is unclassifiable alone


def test_nt424_file_select_denied_before_identity_verified():
    be, _ = nt424_backend()
    denied(be, "00A4000C02E103")        # CC-file SELECT: behind the gate
    denied(be, "00A4040008A00000015100000000")  # any non-NDEF app SELECT


def test_nt424_ndef_app_select_is_the_hce_bootstrap_exception():
    # The NDEF-app SELECT is the reader's session-BINDING command on
    # the phone relay — a pure read, allowed pre-verification so the
    # HCE session can bind before GET_VERSION identifies the card.
    be, _ = nt424_backend(responses=[b"\x90\x00"])
    allowed(be, "00A4040007D276000085010100")


def test_nt424_identity_unlocks_read_profile():
    be, pol = nt424_backend(responses=[GV_P1, GV_P2, GV_P3])
    allowed(be, "9060000000")          # -> 91AF
    allowed(be, "90AF000000")          # -> 91AF
    allowed(be, "90AF000000")          # -> 9100 + UID
    assert pol.verified
    for apdu in ["00A4040007D276000085010100",   # SELECT NDEF app
                 "00A4000C02E103",               # SELECT CC
                 "00A4000C02E104",               # SELECT NDEF file
                 "00B0000002",                   # READ BINARY
                 "00B0000000",                   # READ BINARY (256)
                 "90F50000010200",               # GetFileSettings plain
                 "903C0000010000"]:              # Read_Sig
        allowed(be, apdu)


def test_nt424_wrong_identity_is_fatal():
    other = GV_P3.replace(bytes.fromhex(NT424_UID),
                          bytes.fromhex("04998877665544"))
    be, pol = nt424_backend(responses=[GV_P1, GV_P2, other])
    allowed(be, "9060000000")
    allowed(be, "90AF000000")
    with pytest.raises(hce_relay.PolicyViolation):
        allowed(be, "90AF000000")
    assert not pol.verified
    denied(be, "00A4040007D276000085010100")


@pytest.mark.parametrize("apdu,label", [
    ("9077000000", "AUTH_FIRST (AES)"),
    ("9071000000", "AUTH_NONFIRST"),
    ("90C4000000", "ChangeKey"),
    ("00D6000000", "ISOUpdateBinary (write)"),
    ("908D000000", "WriteData"),
    ("90AD000000", "ReadData native (unnecessary)"),
    ("5F00000000", "ChangeFileSettings / SDM class"),
    ("905C000000", "SetConfiguration (irreversible opts)"),
    ("90F6000000", "GetFileCounters"),
    ("9051000000", "GetCardUID"),
    ("00A4000C02E105", "SELECT proprietary file (key-gated)"),
    ("00B2000000", "unknown INS for this profile"),
    ("F0B0000000", "unknown CLA"),
])
def test_nt424_forbidden_never_reach_transport(apdu, label):
    be, _ = nt424_backend(responses=[GV_P1, GV_P2, GV_P3])
    allowed(be, "9060000000"); allowed(be, "90AF000000"); allowed(be, "90AF000000")
    denied(be, apdu)


# --- J3R180 / JCOP: CPLC-gated read profile ----------------------------------


def cplc_ok(serial="50942720"):
    # EXACT bench shape (ACR read of 50942720, 2026-11-03): the ledger
    # serial sits at bytes 15..18 of the tag-included body.
    body = bytes.fromhex("9f7f2a4790d3214700000000003129") + \
        bytes.fromhex(serial) + bytes.fromhex("678200")
    return body + b"\x90\x00"


JCOP_CPLC_SERIAL = "50942720"


def jcop_backend(serial=JCOP_CPLC_SERIAL, responses=None):
    pol = hce_relay.JcopReadOnlyPolicy(
        expected_id=bytes.fromhex(serial))
    be = backend(pol)
    if responses is not None:
        be._transmit_impl.responses = list(responses)
    return be, pol


def test_jcop_cplc_is_the_only_preverification_command():
    be, pol = jcop_backend(responses=[cplc_ok()])
    denied(be, "00A4040007A000000003000000")   # SELECT before identity
    allowed(be, "80CA9F7F00")                  # GET DATA CPLC
    assert pol.verified
    allowed(be, "00A4040007A000000003000000")  # SELECT now allowed
    allowed(be, "80CA9F7F00")                  # CPLC re-read
    allowed(be, "00CA004F00")                  # applet AID GET DATA


def test_jcop_cplc_identity_mismatch_is_fatal():
    be, pol = jcop_backend(serial=JCOP_CPLC_SERIAL,
                           responses=[cplc_ok("50947320")])
    with pytest.raises(hce_relay.PolicyViolation):
        allowed(be, "80CA9F7F00")
    assert not pol.verified
    denied(be, "00A4040007A000000003000000")


@pytest.mark.parametrize("apdu,label", [
    ("8050000000", "INITIALIZE UPDATE (SC-class, budget-bearing)"),
    ("8482000000", "EXTERNAL AUTHENTICATE"),
    ("0020008100", "VERIFY (PIN)"),
    ("0024008100", "CHANGE REFERENCE DATA"),
    ("80E6000000", "INSTALL"),
    ("80E8000000", "LOAD"),
    ("80D8000000", "DELETE"),
    ("8044000000", "GET STATUS (GP mgmt)"),
    ("80CA00E000", "SC-gated GET DATA 00E0"),
    ("00D6000000", "UPDATE BINARY (write)"),
    ("00E8000000", "unknown INS"),
])
def test_jcop_forbidden_never_reach_transport(apdu, label):
    be, _ = jcop_backend(responses=[cplc_ok()])
    allowed(be, "80CA9F7F00")   # verify identity first
    denied(be, apdu)


# --- backend construction guard ------------------------------------------------


def test_pcsc_backend_refuses_to_construct_without_policy():
    with pytest.raises(hce_relay.RelayError):
        hce_relay.PcscBackend("needle", policy=None)
    # explicit opt-out is the documented escape hatch for owned experiments
    be = hce_relay.PcscBackend("needle", policy=None, unsafe_raw=True)
    assert be.status()["policy"] == "raw-unsafe"


def test_session_dies_loudly_on_policy_violation():
    """A denied APDU must surface as a session error, never a reply."""
    phone, host = __import__("socket").socketpair()
    import threading
    be, _ = nt424_backend()
    journal = hce_relay.JsonlJournal(
        Path(__file__).parent / ".policy-journal-test.jsonl")
    session = hce_relay.RelaySession(host, be, journal=journal)
    session._backend_connected = True   # skip lazy connect (offline test)
    phone.settimeout(5)
    try:
        t = threading.Thread(target=session.run, kwargs={"once": 1},
                             daemon=True)
        t.start()
        phone.send(b"\x00\x02\x00\x00")   # forbidden 0000 C-APDU
        assert phone.recv(16) == b""       # no reply, socket closed
        t.join(timeout=5)
        assert not t.is_alive()
    finally:
        phone.close()
        host.close()
        journal.path.unlink(missing_ok=True)
