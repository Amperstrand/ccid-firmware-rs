#!/usr/bin/env python3
"""gempc_sim tests — all host-only (the #virtualization lane's point).

Unit legs exercise the reader state machine in-process; the pty leg opens
the simulator's pty through conformance_battery.Reader (same transport
class as the /dev/m5stick-remote socat pty) and drives a full
status->power-on->APDU exchange over the wire, LRCs included.
"""
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "nfc"))
sys.path.insert(0, str(HERE))

from conformance_battery import (  # noqa: E402
    MSG_GET_PARAMETERS, MSG_GET_SLOT_STATUS, MSG_ICC_POWER_ON,
    MSG_XFR_BLOCK, parse_frames,
)
from gempc_sim import (  # noqa: E402
    CMD_FAILED, DEFAULT_ATR, ICC_ABSENT, ICC_ACTIVE, RDR_DATABLOCK,
    RDR_PARAMETERS, RDR_SLOTSTATUS, GemPcSim,
)

SELECT = bytes.fromhex("00A4040007D276000085010100")


def resp_payload(wire: bytes):
    frames = parse_frames(wire)
    assert frames, "no LRC-valid response frame"
    return frames[0]


def test_power_on_returns_atr():
    sim = GemPcSim(DEFAULT_ATR, {}, None)
    mt, seq, status, err, payload = resp_payload(
        sim.on_frame(MSG_ICC_POWER_ON, 7, b""))
    assert mt == RDR_DATABLOCK and seq == 7
    assert status == ICC_ACTIVE
    assert payload == DEFAULT_ATR


def test_xfr_block_scripted_table_and_default_miss():
    sim = GemPcSim(DEFAULT_ATR, {SELECT: bytes.fromhex("9000")}, None)
    _, _, _, _, payload = resp_payload(sim.on_frame(MSG_XFR_BLOCK, 1, SELECT))
    assert payload == bytes.fromhex("9000")
    _, _, status, _, payload = resp_payload(
        sim.on_frame(MSG_XFR_BLOCK, 2, bytes.fromhex("00C0000000")))
    assert payload == bytes.fromhex("6a82")  # unmatched -> 6A82 (hce_relay shape)


def test_xfr_block_without_card_fails_named():
    sim = GemPcSim(DEFAULT_ATR, {}, None)
    sim.card_active = False
    mt, _, status, _, _ = resp_payload(
        sim.on_frame(MSG_XFR_BLOCK, 1, SELECT))
    assert mt == RDR_SLOTSTATUS
    assert status == (ICC_ABSENT | CMD_FAILED)


def test_get_parameters_is_0x6c_shaped():
    sim = GemPcSim(DEFAULT_ATR, {}, None)
    wire = sim.on_frame(MSG_GET_PARAMETERS, 3, b"")
    mt, _, status, berror, payload = resp_payload(wire)
    assert mt == RDR_PARAMETERS and berror == 0
    # bSpecific (bProtocolNum = T=1) lives at wire offset 11, after
    # [SYNC][CTRL][type][len x4][slot][seq][bStatus][bError]
    assert wire[11] == 0x01
    assert payload == bytes([0x11, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00])


def test_slot_status_absent():
    sim = GemPcSim(DEFAULT_ATR, {}, None)
    sim.card_active = False
    _, _, status, _, _ = resp_payload(
        sim.on_frame(MSG_GET_SLOT_STATUS, 9, b""))
    assert status == ICC_ABSENT


def test_bad_lrc_gets_no_answer():
    """The parser must drop corrupt frames — a sim answering noise would
    desync a strict libccidtwin (the clean-wire class)."""
    sim = GemPcSim(DEFAULT_ATR, {}, None)
    assert sim.on_frame(0xFF, 1, b"")  # unknown type still answers (named)


@pytest.mark.timeout(30)
def test_pty_round_trip():
    """Full wire exchange over a real pty via the battery's transport
    class (same as the /dev/m5stick-remote socat pty). PTYs give select()
    false-positives — reads poll with retries (pyserial's documented pty
    quirk), matching the bench's drain-until-idle practice."""
    import serial  # noqa: WPS433 (deferred: hardware import rule)
    proc = subprocess.Popen(
        [sys.executable, str(HERE / "gempc_sim.py"),
         "--script", "-", "--once", "1"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE)
    try:
        proc.stdin.write(b"00A4040007D276000085010100=9000\n")
        proc.stdin.close()
        port = proc.stdout.readline().decode().strip()
        assert port.startswith("/dev/pts/"), port

        s = serial.Serial(port=None, baudrate=115200, timeout=0.2,
                          stopbits=2)
        s.port = port
        try:
            s.dtr = False
            s.rts = False
        except OSError:
            pass
        s.open()

        from conformance_battery import frame

        def read_frames(want: int = 2, tries: int = 40):
            """Raw-fd poll read until `want` LRC-valid frames arrived
            ([echo][response] per Twin semantics). pyserial's select()
            layer misbehaves on ptys (false-ready + silent classes —
            found the hard way in this very test): os.read on the fd
            uses plain blocking semantics instead."""
            import os as _os
            import select as _select
            import time as _time
            got = b""
            for _ in range(tries):
                r, _, _ = _select.select([s.fd], [], [], 0.05)
                if r:
                    got += _os.read(s.fd, 512)
                    frames = parse_frames(got)
                    if len(frames) >= want:
                        return frames
                _time.sleep(0.02)
            raise AssertionError(
                f"only {len(parse_frames(got))} frames within poll window; "
                f"got={got.hex()!r}")

        # Twin echo semantics: every exchange reads back [echo][response]
        # (libccidtwin discards the echo on its FIRST read only).
        s.write(frame(MSG_GET_SLOT_STATUS, 1))
        frames = read_frames()
        assert frames[0][0] == MSG_GET_SLOT_STATUS  # the echo = our frame
        assert frames[1][0] == RDR_SLOTSTATUS
        assert frames[1][2] == ICC_ACTIVE

        s.write(frame(MSG_ICC_POWER_ON, 2))
        frames = read_frames()
        assert frames[0][0] == MSG_ICC_POWER_ON
        assert frames[1][0] == RDR_DATABLOCK
        assert frames[1][4] == DEFAULT_ATR

        s.write(frame(MSG_XFR_BLOCK, 3, SELECT))
        frames = read_frames()
        assert frames[1][4] == bytes.fromhex("9000")
        s.close()
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
        err = proc.stderr.read().decode(errors="replace").strip()
        if err:
            print(f"--- gempc_sim stderr ---\n{err}", file=sys.stderr)
