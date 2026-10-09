#!/usr/bin/env python3
"""Cross-reader APDU performance benchmark (issue #87 companion).

Drives every pcscd reader that currently has a card through the same
APDU round-trip workload and reports latency statistics side by side —
the "same card through every reader, against each other" harness.

Read-only by design: the default probe is an empty SELECT (every card
answers 6x xx within one exchange; no file/app is touched).

Usage:
    ./benchmark_readers.py                     # all readers with cards
    ./benchmark_readers.py --readers Cherry,ACR1252
    ./benchmark_readers.py -n 200 --apdu 00A4040002 3F00
"""

import argparse
import statistics
import time

from smartcard.System import readers
from smartcard.util import toBytes, toHexString

WARMUP = 10


def percentiles(xs, ps):
    xs = sorted(xs)
    out = {}
    for p in ps:
        k = min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))
        out[p] = xs[k]
    return out


def bench_reader(reader, apdu: bytes, n: int):
    conn = reader.createConnection()
    t0 = time.perf_counter()
    conn.connect()
    connect_ms = (time.perf_counter() - t0) * 1000
    atr = conn.getATR()

    for _ in range(WARMUP):
        conn.transmit(apdu)
    lat = []
    for _ in range(n):
        t = time.perf_counter()
        _, sw1, sw2 = conn.transmit(apdu)
        lat.append((time.perf_counter() - t) * 1000)
        if sw1 == 0x6C or sw1 == 0x98:
            break  # card insisted on something; don't hammer a bad state
    conn.disconnect()

    p = percentiles(lat, [50, 95, 99])
    return {
        "name": str(reader),
        "atr": toHexString(atr),
        "connect_ms": connect_ms,
        "n": len(lat),
        "mean": statistics.mean(lat),
        "min": min(lat),
        "max": max(lat),
        "p50": p[50],
        "p95": p[95],
        "p99": p[99],
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("-n", "--iterations", type=int, default=100)
    ap.add_argument("--readers", help="comma-separated substring filters")
    ap.add_argument("--apdu", nargs="+", default=["00", "A4", "04", "00", "00"],
                    help="read-only probe APDU hex bytes (default: empty SELECT)")
    args = ap.parse_args()

    apdu = toBytes("".join(args.apdu))
    filters = [f.strip().lower() for f in (args.readers or "").split(",") if f.strip()]

    results = []
    for r in readers():
        if filters and not any(f in str(r).lower() for f in filters):
            continue
        try:
            results.append(bench_reader(r, apdu, args.iterations))
        except Exception as e:
            print(f"skip {r}: {str(e)[:60]}")

    if not results:
        raise SystemExit("no reader with a card answered")

    hdr = f"{'reader':46s} {'n':>4s} {'mean':>7s} {'p50':>7s} {'p95':>7s} {'p99':>7s} {'min':>7s} {'max':>7s} {'conn':>6s}"
    print(hdr)
    print("-" * len(hdr))
    for r in sorted(results, key=lambda x: x["p50"]):
        print(f"{r['name'][:46]:46s} {r['n']:4d} {r['mean']:7.2f} {r['p50']:7.2f} "
              f"{r['p95']:7.2f} {r['p99']:7.2f} {r['min']:7.2f} {r['max']:7.2f} "
              f"{r['connect_ms']:6.1f}")
    print()
    for r in results:
        print(f"  {r['name'][:46]:46s} ATR {r['atr'][:44]}")


if __name__ == "__main__":
    main()
