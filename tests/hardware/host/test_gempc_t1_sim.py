#!/usr/bin/env python3
"""gempc_t1 tests — the endpoint twin against the firmware's own
bench-proven semantics (t1.rs). All host-only."""
import sys
from pathlib import Path

import pytest

try:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from gempc_t1_sim import (T1Endpoint, block, i_pcb, lrc, looks_like_block,
                          r_pcb, s_pcb, RESYNC, IFS, WTX)
except ModuleNotFoundError:
    pytest.skip("t1 twin unavailable", allow_module_level=True)

APDU = bytes.fromhex("00A4040007D276000085010100")
SW9000 = bytes.fromhex("9000")


def send(ep: T1Endpoint, pcb: int, inf: bytes = b""):
    return ep.feed(block(pcb, inf))


def test_i_block_round_trip_first_exchange():
    ep = T1Endpoint()
    kind, out = send(ep, i_pcb(0, False, 0), APDU)
    assert kind == "apdu" and out == APDU
    # response PCB carries piggybacked N(R)=1 (host_ns toggled in feed)
    first = ep.card_response(SW9000)
    assert first == bytes([0, i_pcb(0, False, 1), 2]) + SW9000 + bytes(
        [block(i_pcb(0, False, 1), SW9000)[-1]])


def test_sequence_toggles_both_sides():
    ep = T1Endpoint()
    for round_no in range(4):
        ns = round_no % 2
        kind, _ = send(ep, i_pcb(ns, False, ns), APDU)
        assert kind == "apdu"
        first = ep.card_response(SW9000)
        assert (first[1] >> 6) & 1 == ns  # our N(S) mirrors the exchange


def test_chaining_acks_pieces_then_assembles():
    ep = T1Endpoint()
    half = APDU[:6]
    kind, out = send(ep, i_pcb(0, True, 0), half)
    assert kind == "imm" and out == block(r_pcb(0, 0))  # §11.6.3 chain ACK
    kind, out = send(ep, i_pcb(0, False, 0), APDU[6:])
    assert kind == "apdu" and out == APDU


def test_response_chaining_splits_at_ifsd():
    ep = T1Endpoint()
    ep.send_ifsc = 16  # as if the host negotiated a small IFSD
    kind, _ = send(ep, i_pcb(0, False, 0), APDU)
    assert kind == "apdu"
    first = ep.card_response(bytes(range(40)))
    assert first[1] & 0x20  # M bit set
    assert first[2] == 16   # chunked at the negotiated size
    # 40 bytes at ifsc=16 = M,M,final — two R-ACKs consume the chain
    kind, second = ep.feed(block(r_pcb(1, 0)))
    assert kind == "imm" and second[1] & 0x20        # still more
    kind, third = ep.feed(block(r_pcb(1, 0)))
    assert kind == "imm" and third[1] & 0x20 == 0    # final block


def test_sequence_violation_requests_retransmit():
    ep = T1Endpoint()
    kind, out = send(ep, i_pcb(1, False, 0), APDU)  # wrong ns (expected 0)
    assert kind == "imm" and out == block(r_pcb(0, 0))  # §11.5.3


def test_retry_idempotent_duplicate_final_replays_cache():
    """#101: the host re-sending the SAME final I-block (our answer was
    lost) gets the cached first frame — never a second execution."""
    ep = T1Endpoint()
    send(ep, i_pcb(0, False, 0), APDU)
    ep.card_response(SW9000)
    kind, out = send(ep, i_pcb(0, False, 0), APDU)  # duplicate
    assert kind == "imm"
    assert out == ep.retry_first_frame  # the cached answer


def test_resync_resets_counters():
    ep = T1Endpoint()
    send(ep, i_pcb(0, False, 0), APDU)
    ep.card_response(SW9000)
    kind, out = send(ep, s_pcb(RESYNC, False))
    assert kind == "imm" and out == block(s_pcb(RESYNC, True))
    assert ep.ns == ep.host_ns == 0
    kind, _ = send(ep, i_pcb(0, False, 0), APDU)  # counters back at 0
    assert kind == "apdu"


def test_ifs_request_bounds_sends():
    ep = T1Endpoint()
    kind, out = send(ep, s_pcb(IFS, False), bytes([32]))
    assert kind == "imm" and out == block(s_pcb(IFS, True), bytes([32]))
    assert ep.send_ifsc == 32


def test_wtx_request_answered_with_echo():
    ep = T1Endpoint()
    kind, out = send(ep, s_pcb(WTX, False), bytes([2]))
    assert kind == "imm" and out == block(s_pcb(WTX, True), bytes([2]))


def test_r_block_retransmits_last():
    ep = T1Endpoint()
    send(ep, i_pcb(0, False, 0), APDU)
    first = ep.card_response(SW9000)
    kind, out = ep.feed(block(r_pcb(1, 0)))
    assert kind == "imm" and out == first  # §11.5.3.3


def test_bad_lrc_is_rejected():
    assert not looks_like_block(bytes([0, 0, 1, 0x42, 0xFF]))
    ep = T1Endpoint()
    raw = bytearray(block(i_pcb(0, False, 0), APDU))
    raw[-1] ^= 0xFF
    kind, out = ep.feed(bytes(raw))
    assert kind == "imm" and out[1] & 0xC0 == 0x80  # an R-block answer


# --- adversary profiles (host-stack characterization instrument) --------------

def test_adversary_wtx_storm_frames_are_stream_legal():
    from gempc_t1_sim import T1Adversary
    ep = T1Endpoint()
    adv = T1Adversary("wtx-storm:3", ep)
    kind, _ = ep.feed(block(i_pcb(0, False, 0), APDU))
    assert kind == "apdu"
    wire = adv.answer(APDU, SW9000)
    wtx = block(s_pcb(WTX, False), bytes([3]))
    assert wire.startswith(wtx + wtx + wtx)  # 3x S(WTX req, m=3)
    assert wire.endswith(block(i_pcb(0, False, 1), SW9000))  # then the answer


def test_adversary_seq_violation_flips_ns_then_converges():
    from gempc_t1_sim import T1Adversary
    ep = T1Endpoint()
    adv = T1Adversary("seq-violation", ep)
    ep.feed(block(i_pcb(0, False, 0), APDU))
    bad = adv.answer(APDU, SW9000)
    assert bad[1] & 0x40 != block(i_pcb(0, False, 1), SW9000)[1] & 0x40
    assert lrc(bad[:-1]) == bad[-1]  # still LRC-valid (subtle, not corrupt)
    ep.feed(block(s_pcb(RESYNC, False)))  # host resyncs
    ep.feed(block(i_pcb(0, False, 0), APDU))
    good = adv.answer(APDU, SW9000)
    assert good == block(i_pcb(0, False, 1), SW9000)  # clean after reset
