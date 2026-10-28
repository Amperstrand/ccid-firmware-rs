"""Clock-controlled tests for the nucula-ccid-broker power-off shim's
stale-partial flush (PR #483, Codex P1): stale partials are dropped across
idle gaps; valid frames fragmented by delivery within the gap survive.

The shim's host-side timing contract is reference-anchored: libccidtwin's
WriteSerial emits each GemPC command in a single write()
(reference/CCID/src/ccid_serial.c), so application-layer mid-frame pauses
do not exist — an idle gap can only precede a NEW frame.
"""
import importlib.util
import sys
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).parents[3]))
spec = importlib.util.spec_from_file_location(
    "broker", Path(__file__).parents[3] / "host-tools" / "nucula-ccid-broker.py")
broker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(broker)


def _gss(seq: int) -> bytes:
    body = bytes([0x03, 0x06, 0x65, 0x00, 0x00, 0x00, 0x00, 0x00,
                  seq, 0x00, 0x00, 0x00])
    lrc = 0
    for b in body:
        lrc ^= b
    return body + bytes([lrc])


def test_stale_lone_sync_dropped_across_gap():
    t = [1000.0]
    with mock.patch.object(broker.time, "monotonic", lambda: t[0]):
        shim = broker.PowerOffShim()
        out, _ = shim.process(b"\x03")          # lone SYNC
        assert out == b""
        t[0] += 0.30                             # idle past the cutoff
        out, _ = shim.process(_gss(1))           # clean new frame
    assert out == _gss(1), "probe after a stale partial must parse CLEAN"


def test_valid_frame_fragmented_within_gap_survives():
    t = [2000.0]
    frame = _gss(2)
    with mock.patch.object(broker.time, "monotonic", lambda: t[0]):
        shim = broker.PowerOffShim()
        out, _ = shim.process(frame[:5])         # delivery-level split
        assert out == b""
        t[0] += 0.01                             # well within the cutoff
        out, _ = shim.process(frame[5:])
    assert out == frame, "a frame split by kernel delivery must survive"


def test_frame_split_exactly_at_boundary_semantics():
    # frames arriving back-to-back with no gap never flush mid-stream
    t = [3000.0]
    with mock.patch.object(broker.time, "monotonic", lambda: t[0]):
        shim = broker.PowerOffShim()
        out, _ = shim.process(_gss(3) + _gss(4))
    assert out == _gss(3) + _gss(4)
