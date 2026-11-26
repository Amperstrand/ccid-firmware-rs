#!/usr/bin/env python3
"""conformance_sim — the battery's 10-case table against gempc_sim,
ZERO hardware (the #551 diagnosis class, host-only).

Spawns the simulator, binds the battery's own Reader to its pty, runs
conformance_battery() — the same framing/LRC/state-machine cases the
bench runs against real readers, with per-case verdicts. The
cross-reader comparison degenerates to one column; the value is the
regression surface: every battery case now runs on any host with
pyserial, no bench, no pcscd, no board.
"""
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from conformance_battery import (  # noqa: E402
    Reader, conformance_battery, fuzz_battery, slot_status_soak,
)


def main() -> int:
    sim = subprocess.Popen(
        [sys.executable, str(HERE.parent / "host" / "gempc_sim.py")],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        port = sim.stdout.readline().decode().strip()
        if not port.startswith("/dev/pts/"):
            print(f"sim failed to provide a pty: {port!r}")
            return 2
        print(f"gempc_sim pty: {port}")
        reader = Reader("gempc-sim", port)
        readers = {"gempc-sim": reader}
        ok = conformance_battery(readers)
        # The bench legs, host-only and bounded: the framing fuzz
        # (malformed frames must never wedge — every case ends with a
        # valid GetSlotStatus) and the GSS soak (state stability).
        ok = fuzz_battery(readers, rounds=25) and ok
        ok = slot_status_soak(readers, count=100) and ok
        reader.close()
        print("SIM BATTERY VERDICT:", "PASS" if ok else "FAIL")
        return 0 if ok else 1
    finally:
        sim.terminate()
        try:
            sim.wait(timeout=5)
        except subprocess.TimeoutExpired:
            sim.kill()


if __name__ == "__main__":
    sys.exit(main())
