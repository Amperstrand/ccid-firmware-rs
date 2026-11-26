#!/usr/bin/env python3
"""wire_diff — #540 wire-oracle comparison: two JSONL wire captures,
byte-level per-exchange, timing-agnostic.

Usage:
  python3 wire_diff.py known-good.jsonl device-under-test.jsonl

Each capture is gempc_sim --capture / the device-side equivalent (the
#540 harness: same APDU script through the sim and the real m5stick;
the diff names the FIRST divergent exchange — the byte where the
device's answer stopped matching the oracle).
Exit: 0 identical (byte-for-byte on every exchange), 1 divergent
(first difference reported), 2 usage/capture error.
"""
import json
import sys
from pathlib import Path


def load(path: str) -> list[dict]:
    events = []
    for n, line in enumerate(Path(path).read_text().splitlines(), 1):
        line = line.strip()
        if not line:
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError as e:
            raise SystemExit(f"{path}:{n}: bad JSONL ({e})") from None
    return events


def frames(events: list[dict]) -> list[tuple[str, str]]:
    """The comparable sequence: (direction, hex) per frame, timestamps
    dropped (timing-agnostic by design — the #540 question is BYTES)."""
    return [(e["event"], e["data"]) for e in events if "event" in e
            and e["event"] in ("rx", "tx")]


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(f"usage: {argv[0]} <known-good.jsonl> <dut.jsonl>",
              file=sys.stderr)
        return 2
    good, dut = frames(load(argv[1])), frames(load(argv[2]))
    if not good or not dut:
        print("error: a capture has no rx/tx frames", file=sys.stderr)
        return 2
    for i, (g, d) in enumerate(zip(good, dut)):
        if g != d:
            print(f"DIVERGENCE at frame {i}:")
            print(f"  known-good: {g[0]} {g[1]}")
            print(f"  dut:        {d[0]} {d[1]}")
            return 1
    if len(good) != len(dut):
        shorter = "known-good" if len(good) < len(dut) else "dut"
        print(f"DIVERGENCE: length mismatch ({len(good)} vs {len(dut)} "
              f"frames; {shorter} ran longer)")
        return 1
    print(f"MATCH: {len(good)} frames byte-for-byte (timing-agnostic)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
