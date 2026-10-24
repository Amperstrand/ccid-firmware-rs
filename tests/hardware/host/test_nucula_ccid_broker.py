"""Regression tests for the nucula CCID broker (host side, issue #375).

The 2026-10-21 wedge was attributed to firmware; it is actually the
PowerOffShim's frame scanner: a truncated/garbage SYNC anchor with an
insane implied dwLength makes split_frames break without rescanning,
so the shim buffers and forwards NOTHING forever — the board never
sees a byte and the reader looks dead.
"""

import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
BROKER = os.path.join(HERE, "..", "..", "..", "host-tools", "nucula-ccid-broker.py")


def load_broker():
    spec = importlib.util.spec_from_file_location("nucula_ccid_broker", BROKER)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["nucula_ccid_broker"] = mod
    spec.loader.exec_module(mod)
    return mod


def gss_frame_bytes(seq: int) -> bytes:
    mod = load_broker()
    return mod.gss_frame(seq)


class ShimHarness:
    def __init__(self):
        mod = load_broker()
        mod.os.environ["POWER_OFF_SHIM"] = "1"
        self.shim = mod.PowerOffShim()

    def feed(self, data: bytes):
        return self.shim.process(data)


def test_gss_frame_shape():
    mod = load_broker()
    f = mod.gss_frame(0)
    assert f[0] == 0x03 and f[1] == 0x06 and len(f) >= 13


def test_valid_frame_forwards():
    h = ShimHarness()
    fwd, _ = h.feed(gss_frame_bytes(1))
    assert len(fwd) >= 13, "a complete valid frame must forward"


def test_truncated_header_then_probe_forwards():
    """#375 reproducer, host side: the 4-byte cut frame `03 06 65 01`
    followed by a valid GetSlotStatus probe must NOT wedge the shim —
    the probe has to reach the board."""
    h = ShimHarness()
    fwd, _ = h.feed(bytes([0x03, 0x06, 0x65, 0x01]))
    assert fwd == b"", "cut frame alone forwards nothing (partial)"
    fwd, _ = h.feed(gss_frame_bytes(2))
    assert len(fwd) >= 13, (
        "after a garbage-dwLength anchor the shim must resync and "
        "forward the subsequent valid frame (was: buffered forever)"
    )


def test_overcap_anchor_band_is_garbage():
    """#450: anchors implying dwLength 262..271 (above the true payload
    cap 261, inside the old wrong 271-as-payload belief) must resync
    like any garbage — under SANE_MAX=13+271 they counted as 'sane
    partials' and blocked the scan until 278 bytes arrived."""
    h = ShimHarness()
    hdr = bytearray(10)
    hdr[0] = 0x65
    hdr[1:5] = (265).to_bytes(4, "little")
    overcap = bytes([0x03, 0x06]) + bytes(hdr)
    fwd, _ = h.feed(overcap)
    assert fwd == b"", "over-cap anchor alone forwards nothing"
    fwd, _ = h.feed(gss_frame_bytes(4))
    assert len(fwd) >= 13, "over-cap anchor must not block the next valid frame"


def test_garbage_then_valid_poweroff_still_shimmed():
    """The shim's real job must survive the resync fix: an IccPowerOff
    (type 0x65, per GemPC) inside a VALID frame is answered locally,
    not forwarded."""
    mod = load_broker()
    h = ShimHarness()
    # Build a valid frame with the broker's IccPowerOff type: SYNC ACK
    # + 10-byte CCID header (type = MSG_ICC_POWER_OFF) + payload + LRC.
    hdr = bytearray(10)
    hdr[0] = mod.MSG_ICC_POWER_OFF
    body = bytes([0x03, 0x06]) + bytes(hdr) + bytes([0x00])
    lrc = 0
    for b in body:
        lrc ^= b
    frame = body + bytes([lrc])
    fwd, synth = h.feed(frame)
    assert fwd == b"", "power-off must be suppressed (not forwarded)"
    assert len(synth) >= 13, "a synthetic power-off answer must come back"
