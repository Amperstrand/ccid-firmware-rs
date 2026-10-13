"""Sequential-APDU stream throughput: APDUs/sec + effective bytes/sec.

Streams (both read-only SELECT-by-DF-name probes; see perflib.py):
* small — exactly 6-byte command APDUs,
* large — exactly 255-byte command APDUs (wire exercise; on vpcd these
  answer SW 64 00 above the documented ~245 B NCI single-packet cap —
  runbook "GP" — and are counted as transport exchanges).

Metrics per size:
* ``stream_{size}_apdu_ms`` — per-APDU latency stats of the stream,
* ``throughput_{size}_apdus_per_sec`` — completed exchanges / wall time,
* ``throughput_{size}_bytes_per_sec`` — (command bytes + response bytes
  incl. SW) / wall time.

The stream is closed and sequential (no sleeps between APDUs), so each
sample is a complete issue->completion interval and the aggregate rate
is honest by construction — no coordinated omission (Gil Tene; links in
perflib.py header / README.md). One connection per reader for the whole
test: never power-cycled mid-sample. Same 3-strike error policy as the
RTT test.
"""

import time

import pytest

from .perflib import (STREAM_LARGE_APDU, STREAM_SMALL_APDU,
                      connect_reader_obj, no_gc, transmit)

STREAMS = (("small", STREAM_SMALL_APDU), ("large", STREAM_LARGE_APDU))

#: consecutive transmit failures before abandoning a stream
ERROR_STRIKES = 3


def _run_stream(conn, apdu, warmup, n):
    """One sequential stream: warmup exchanges, then n timed ones.

    Returns (per_apdu_ms, response_bytes) with response SW bytes (2)
    counted per completed exchange.
    """
    for _ in range(warmup):
        try:
            transmit(conn, apdu)
        except Exception:
            pass  # warmup only paces the transport; failures are not fatal

    per_ms, resp_bytes, sw_counts, strikes = [], 0, {}, 0
    with no_gc():
        t_start = time.perf_counter()
        for _ in range(n):
            try:
                t0 = time.perf_counter()
                resp, sw = transmit(conn, apdu)
                per_ms.append((time.perf_counter() - t0) * 1000.0)
            except Exception as e:  # transport-level failure
                strikes += 1
                print(f"    transmit error ({strikes}/{ERROR_STRIKES}): "
                      f"{str(e)[:80]}")
                if strikes >= ERROR_STRIKES:
                    break
                continue
            strikes = 0
            resp_bytes += len(resp) + 2
            sw_key = f"{sw:04X}"
            sw_counts[sw_key] = sw_counts.get(sw_key, 0) + 1
        t_end = time.perf_counter()
    return per_ms, resp_bytes, sw_counts, t_end - t_start


def test_throughput(reader_target, ledger):
    for t in reader_target:
        p = t["params"]
        print(f"\n[perf] throughput: {t['name']} "
              f"(warmup={p['warmup']} + {p['iterations']} APDUs/stream)")
        conn, _ = connect_reader_obj(t["reader_obj"])
        try:
            for label, apdu in STREAMS:
                per_ms, resp_bytes, sw_counts, elapsed_s = _run_stream(
                    conn, apdu, p["warmup"], p["iterations"])
                print(f"  [perf] {t['name']} {label} ({len(apdu)} B cmd): "
                      f"n={len(per_ms)} SW {sw_counts} elapsed={elapsed_s:.3f}s")
                if not per_ms or elapsed_s <= 0:
                    pytest.fail(f"{t['name']}: no {label} stream samples")
                ledger.record_samples(t["name"], t["transport"],
                                      f"stream_{label}_apdu_ms", per_ms)
                ledger.record_aggregate(
                    t["name"], t["transport"],
                    f"throughput_{label}_apdus_per_sec",
                    len(per_ms) / elapsed_s, len(per_ms))
                total_bytes = len(apdu) * len(per_ms) + resp_bytes
                ledger.record_aggregate(
                    t["name"], t["transport"],
                    f"throughput_{label}_bytes_per_sec",
                    total_bytes / elapsed_s, len(per_ms))
        finally:
            try:
                conn.disconnect()
            except Exception:
                pass
