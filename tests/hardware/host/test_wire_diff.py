#!/usr/bin/env python3
"""wire_diff + gempc_sim --capture tests — the #540 oracle harness."""
import json
import subprocess
import sys
from pathlib import Path

import pytest

try:
    import serial  # noqa: F401
    HERE = Path(__file__).resolve().parent
    sys.path.insert(0, str(HERE))
    from wire_diff import frames, main as diff_main
except ModuleNotFoundError:
    pytest.skip("pyserial unavailable", allow_module_level=True)


def write_cap(path, events):
    Path(path).write_text(
        "\n".join(json.dumps(e) for e in events) + "\n")


def caps(tmp_path):
    good = [{"event": "session", "t": 1}, {"event": "rx", "t": 2, "data": "0306"},
            {"event": "tx", "t": 3, "data": "8400"}]
    return tmp_path / "g.jsonl", tmp_path / "d.jsonl", good


def test_match_is_byte_for_byte_ignoring_time(tmp_path):
    g, d, good = caps(tmp_path)
    write_cap(g, good)
    write_cap(d, [dict(e, t=e["t"] + 99) for e in good])  # same, later
    assert diff_main(["wire_diff", str(g), str(d)]) == 0


def test_single_byte_divergence_is_named(tmp_path):
    g, d, good = caps(tmp_path)
    bad = [dict(e) for e in good]
    bad[2] = {"event": "tx", "t": 3, "data": "8500"}
    write_cap(g, good)
    write_cap(d, bad)
    assert diff_main(["wire_diff", str(g), str(d)]) == 1


def test_length_mismatch_is_a_divergence(tmp_path):
    g, d, good = caps(tmp_path)
    write_cap(g, good)
    write_cap(d, good[:-1])  # DUT ran shorter
    assert diff_main(["wire_diff", str(g), str(d)]) == 1


def test_frames_drops_session_and_keeps_order():
    evs = [{"event": "session"}, {"event": "rx", "data": "aa"},
           {"event": "tx", "data": "bb"}, {"event": "note"}]
    assert frames(evs) == [("rx", "aa"), ("tx", "bb")]


@pytest.mark.timeout(30)
def test_capture_mode_records_every_exchange(tmp_path):
    """gempc_sim --capture: JSONL with session + rx/tx per exchange —
    the oracle side of the #540 harness."""
    cap = tmp_path / "cap.jsonl"
    proc = subprocess.Popen(
        [sys.executable, str(HERE / "gempc_sim.py"),
         "--respond", "9000", "--once", "1", "--capture", str(cap)],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        port = proc.stdout.readline().decode().strip()
        assert port.startswith("/dev/pts/"), port
        s = serial.Serial(port=port, baudrate=115200, timeout=0.5,
                          stopbits=2)
        sys.path.insert(0, str(HERE.parent / "nfc"))
        from conformance_battery import MSG_GET_SLOT_STATUS, frame
        s.write(frame(MSG_GET_SLOT_STATUS, 1))
        import time
        # poll until the capture carries the exchange (pty timing is
        # racy under load; fixed sleeps flake — the round-trip lesson)
        for _ in range(40):
            if cap.exists() and cap.read_text().count("rx") >= 1 \
                    and cap.read_text().count("tx") >= 1:
                break
            time.sleep(0.05)
        s.close()
        time.sleep(0.4)  # --once drain grace
        events = [json.loads(l) for l in cap.read_text().splitlines()]
        kinds = [e["event"] for e in events]
        assert kinds[0] == "session"
        assert kinds.count("rx") >= 1 and kinds.count("tx") >= 1
        assert events[1]["data"].startswith("0306")  # the frame we sent
    finally:
        proc.terminate()
        proc.wait(timeout=5)
