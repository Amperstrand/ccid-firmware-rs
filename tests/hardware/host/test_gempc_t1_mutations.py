#!/usr/bin/env python3
"""T=1 twin robustness corpus — every single-bit mutation of a valid
block must be handled: LRC-rejected (R-block answer), length-rejected,
or silently resynced — never an exception, never a hang, never a
silent accept of a corrupted INF. Deterministic corpus (no property
framework): the I-block space is small enough to exhaust bitwise.
"""
import sys
from pathlib import Path

import pytest

try:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from gempc_sim import DEFAULT_ATR, GemPcSim
    from gempc_t1_sim import (T1Endpoint, block, i_pcb, lrc,
                              looks_like_block, r_pcb)
except ModuleNotFoundError:
    pytest.skip("t1 twin unavailable", allow_module_level=True)

SELECT = bytes.fromhex("00A4040007D276000085010100")
VALID = block(i_pcb(0, False, 0), SELECT)


def bits_of(data: bytes):
    for i, b in enumerate(data):
        for bit in range(8):
            yield i, bit


@pytest.mark.timeout(60)
def test_every_single_bit_flip_is_contained():
    """Flip each bit of a valid first-exchange I-block: the endpoint
    must answer (R/error) or reject — never raise. LRC-violating flips
    land in _bad(); the PCB/LEN/LRC-byte flips exercise the parser's
    structural paths."""
    for i, bit in bits_of(VALID):
        mutated = bytearray(VALID)
        mutated[i] ^= 1 << bit
        ep = T1Endpoint()
        try:
            kind, out = ep.feed(bytes(mutated))
        except Exception as e:  # noqa: BLE001 — the contract under test
            pytest.fail(f"bit {i}.{bit} raised {e!r}")
            continue
        assert kind in ("imm", "apdu"), (i, bit, kind)
        if kind == "imm":
            assert isinstance(out, bytes) and out, (i, bit)


@pytest.mark.timeout(60)
def test_flipped_inf_never_silently_passes():
    """A corrupted INF (not caught by LRC — we recompute LRC) must
    still assemble: the endpoint is a TRANSPORT twin, not a checksum
    oracle; the reassembled APDU differs → the CARD layer answers
    differently (6A82 on table miss). This documents the layering."""
    for i in range(3, 3 + len(SELECT)):  # INF region only
        mutated = bytearray(VALID)
        mutated[i] ^= 0xFF
        head = bytes(mutated[:-1])
        mutated[-1] = lrc(head)
        ep = T1Endpoint()
        kind, out = ep.feed(bytes(mutated))
        assert kind == "apdu" and out != SELECT  # delivered, different


@pytest.mark.timeout(30)
def test_boundary_lengths_are_structural_not_crashes():
    ep = T1Endpoint()
    # LEN lies (says 5, body is 12): structural reject
    bad = bytes([0x00, i_pcb(0, False, 0), 5]) + SELECT + bytes([0x00])
    kind, out = ep.feed(bad)
    assert kind == "imm"
    # LEN = 255 with short body: reject (partial/bad)
    kind, out = ep.feed(bytes([0, i_pcb(0, False, 0), 255, 1, 2, 3]))
    assert kind == "imm"
    # 3-byte truncation: reject
    kind, out = ep.feed(bytes([0, i_pcb(0, False, 0), 3]))
    assert kind == "imm"
    # looks_like_block agrees on the structural classes
    assert not looks_like_block(bad)
    assert not looks_like_block(bytes([0, 0, 1, 0x42, 0xFF]))


@pytest.mark.timeout(30)
def test_oversize_ifsc_is_rejected():
    """LEN > MAX_IFSC (254) in a structurally complete block: the
    endpoint rejects rather than allocating a huge chain."""
    payload = bytes(300)
    wire = bytes([0, i_pcb(0, False, 0), 255]) + payload[:255] + bytes(
        [lrc(bytes([0, i_pcb(0, False, 0), 255]) + payload[:255])])
    ep = T1Endpoint()
    kind, out = ep.feed(wire)
    assert kind == "imm"  # rejected with an R-block answer


@pytest.mark.timeout(60)
def test_sim_dispatch_survives_garbage_payloads():
    """The gempc_sim card dispatch (block-vs-APDU) on adversarial
    XfrBlock payloads: never raises, always answers a frame."""
    sim = GemPcSim(DEFAULT_ATR, {}, None)
    sim.on_frame(0x62, 1, b"")  # power on (lifecycle)
    corpus = [
        b"", b"\x00", b"\x00\x00", b"\x00\x00\x00",
        bytes(range(256)), b"\x00" * 4096,
        bytes([0, 0, 255]) + b"\xAA" * 300,
    ]
    for payload in corpus:
        resp = sim.on_frame(0x6F, 2, payload)
        assert isinstance(resp, bytes) and resp.startswith(bytes([0x03])), (
            payload[:8].hex())
