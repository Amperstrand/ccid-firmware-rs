#!/usr/bin/env python3
"""Escape 0xD3 I2C-cadence probe — the #149 P3 bench-compare driver.

Firmware contract (firmware/esp32-ccid/src/ccid_handler.rs, issue #120):
`Escape 0xD3 [D3, N]` times N back-to-back ComIrqReg reads — the exact
transaction the MFRC522 transceive poll loop performs — and answers a
12-byte payload: ``[took_us u32 LE][reads u32 LE][fwt_ms u32 LE]``.
``took_us == 0xFFFFFFFF`` means the active frontend has no probe hook.

What this tool is for (#149 P3 acceptance, issue #255): the same sweep
on the LEGACY-i2c image and on the driver_ng image must land in the
same µs-per-read class — the fused repeated-START `write_read` path is
the wire-shape parity the migration promised. STEP 0 in #255 is to run
this against the pre-#242 image BEFORE anyone reflashes it.

Bench discipline (AGENTS.md): hold the `m5stick` place before touching
the port; the GemPC wire is exclusive — pcscd must not hold the port
(``flock /run/lock/bench-pcscd.lock`` around any pcscd stop/start).
The safe opener (dtr/rts deasserted BEFORE open — the DTR-latch rule,
Codex #184) and the >=4 s post-open settle are mandatory (#245
re-confirmed: without the settle the first commands get no response).

Usage:
    m5stick_cadence_probe.py --sweep                 # default N set, 3 reps
    m5stick_cadence_probe.py --reads 1,100,1000 -r 5
    m5stick_cadence_probe.py --sweep --out base.json
    m5stick_cadence_probe.py --compare base.json drvng.json [--ratio 1.25]

`--compare` needs no hardware: it diffs two sweep JSONs and gates on
the median µs-per-read ratio (candidate/baseline).
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from board_identity import escape_frame, parse_frames  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from serial_ports import resolve  # noqa: E402

DEFAULT_SWEEP = (1, 10, 100, 1000)
DEFAULT_REPS = 3
SETTLE_SECS = 4.0
PROBE_FAILED = 0xFFFFFFFF
EXCHANGE_BUDGET = 8.0  # N=1000 at ~0.1 ms/read + framing headroom
TIMEOUT_PER_READ = 0.2


def decode_cadence(payload: bytes) -> tuple[int, int, int]:
    """12-byte reply → (took_us, reads, fwt_ms). Fails loudly on shape
    (issue #110: a wrong answer must fail, not pass as data)."""
    if len(payload) != 12:
        raise ValueError(f"cadence payload is {len(payload)} B, want 12")
    took_us, reads, fwt_ms = (
        int.from_bytes(payload[0:4], "little"),
        int.from_bytes(payload[4:8], "little"),
        int.from_bytes(payload[8:12], "little"),
    )
    return took_us, reads, fwt_ms


class CadenceWire:
    """One persistent safe-opener connection for the whole sweep — no
    per-exchange reconnects (re-opens re-arm the settle window and add
    host-side noise; the firmware measures on-device either way)."""

    def __init__(self, port: str, settle: float = SETTLE_SECS):
        import serial as pyserial

        s = pyserial.Serial()
        s.port = port
        s.baudrate = 115200
        s.timeout = 0.15
        s.dtr = False  # BEFORE open — the DTR-latch rule
        s.rts = False
        s.open()
        self._s = s
        time.sleep(settle)
        s.reset_input_buffer()

    def cadence(self, reads: int) -> tuple[int, int, int]:
        self._s.timeout = min(TIMEOUT_PER_READ * reads + 1.0, EXCHANGE_BUDGET)
        self._s.write(escape_frame(bytes([0xD3, reads])))
        self._s.timeout = 0.2
        buf = bytearray()
        idle = 0
        t0 = time.time()
        while time.time() - t0 < EXCHANGE_BUDGET and idle < 2:
            chunk = self._s.read(8192)
            if chunk:
                buf += chunk
                idle = 0
            else:
                idle += 1
        replies = [f for f in parse_frames(bytes(buf)) if f[0] == 0x83]
        if not replies:
            raise TimeoutError(f"no escape reply for N={reads} (raw {len(buf)} B)")
        return decode_cadence(replies[-1][1])

    def close(self) -> None:
        self._s.close()


def run_sweep(port: str, sweep: list[int], reps: int, settle: float) -> dict:
    wire = CadenceWire(port, settle=settle)
    try:
        results = []
        for n in sweep:
            per_rep = []
            fwt_ms = None
            for _rep in range(reps):
                took_us, reads, fwt_ms = wire.cadence(n)
                if took_us == PROBE_FAILED:
                    raise RuntimeError(
                        f"N={n}: firmware reports probe FAILED "
                        "(frontend has no i2c_timing_probe hook)"
                    )
                if reads != n:
                    raise RuntimeError(f"N={n}: reply reports reads={reads}")
                per_rep.append(took_us)
                time.sleep(0.1)
            assert fwt_ms is not None
            results.append(
                {
                    "reads": n,
                    "took_us": per_rep,
                    "us_per_read_median": statistics.median(per_rep) / n,
                    "fwt_ms": fwt_ms,
                }
            )
            print(
                f"N={n:>5}  median {statistics.median(per_rep) / n:8.1f} us/read  "
                f"(reps: {per_rep})  fwt {fwt_ms} ms"
            )
        return {"sweep": results}
    finally:
        wire.close()


def compare(baseline: dict, candidate: dict, ratio_gate: float) -> int:
    base = {r["reads"]: r["us_per_read_median"] for r in baseline["sweep"]}
    cand = {r["reads"]: r["us_per_read_median"] for r in candidate["sweep"]}
    worst = 0.0
    failures = []
    print(f"{'N':>7} {'baseline us/r':>14} {'driver_ng us/r':>14} {'ratio':>7}")
    for n in sorted(set(base) & set(cand)):
        ratio = cand[n] / base[n] if base[n] else float("inf")
        worst = max(worst, ratio)
        flag = "" if ratio <= ratio_gate else "  <-- OVER GATE"
        print(f"{n:>7} {base[n]:>14.1f} {cand[n]:>14.1f} {ratio:>7.2f}{flag}")
        if ratio > ratio_gate:
            failures.append(n)
    for n in sorted(set(base) ^ set(cand)):
        print(f"N={n}: only in {'candidate' if n in cand else 'baseline'} — not compared")
    if failures:
        print(f"FAIL: us/read ratio over {ratio_gate} at N={failures}")
        return 1
    print(f"PASS: worst ratio {worst:.2f} <= {ratio_gate}")
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--port", help="override serial_ports.resolve('m5stick')")
    p.add_argument("--reads", help="comma-separated N set (default 1,10,100,1000)")
    p.add_argument("-r", "--reps", type=int, default=DEFAULT_REPS)
    p.add_argument("--settle", type=float, default=SETTLE_SECS)
    p.add_argument("--out", help="write the sweep JSON here")
    p.add_argument("--sweep", action="store_true", help="run a sweep (default action)")
    p.add_argument("--compare", nargs=2, metavar=("BASELINE", "CANDIDATE"),
                   help="diff two sweep JSONs, no hardware needed")
    p.add_argument("--ratio", type=float, default=1.25,
                   help="compare gate: candidate/baseline median us-per-read")
    args = p.parse_args(argv)

    if args.compare:
        with open(args.compare[0]) as f:
            base = json.load(f)
        with open(args.compare[1]) as f:
            cand = json.load(f)
        return compare(base, cand, args.ratio)

    sweep = [int(x) for x in args.reads.split(",")] if args.reads else list(DEFAULT_SWEEP)
    result = run_sweep(args.port or resolve("m5stick"), sweep, args.reps, args.settle)
    result["source"] = {
        "tool": "m5stick_cadence_probe.py",
        "sweep": sweep,
        "reps": args.reps,
        "captured_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    if args.out:
        Path(args.out).write_text(json.dumps(result, indent=2) + "\n")
        print(f"wrote {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
