"""Time-to-ATR after connect, per reader.

Metric ``atr_latency_ms``: wall time of perflib.connect_reader_obj()
(SCardConnection create + SCardConnect + getATR) — from initiating the
connect to holding the ATR bytes. Each sample is a full reconnect
cycle; the disconnect happens BETWEEN samples, never mid-sample
(security: no power-cycling inside a timed region).

Methodology (perflib.py header, full links in README.md): first
``warmup`` samples are discarded (pcscd/driver cold-start + card ATR
negotiation dominate them); timings use time.perf_counter; the cyclic
GC is disabled inside each timed region; p50/p90/p99/max land in the
ledger. Sample count is per reader class (PARAMS) because a vpcd
reconnect costs seconds against microseconds on USB.
"""

import time

import pytest

from .perflib import RECONNECT_SETTLE_S, connect_reader_obj, no_gc


def test_atr_latency(reader_target, ledger):
    for t in reader_target:
        p = t["params"]
        print(f"\n[perf] ATR latency: {t['name']} "
              f"(warmup={p['warmup']} + samples={p['atr_samples']})")
        samples = []
        conn = None
        try:
            for i in range(p["warmup"] + p["atr_samples"]):
                if conn is not None:
                    conn.disconnect()
                    time.sleep(RECONNECT_SETTLE_S)
                with no_gc():
                    t0 = time.perf_counter()
                    conn, _atr = connect_reader_obj(t["reader_obj"])
                    dt_ms = (time.perf_counter() - t0) * 1000.0
                if i >= p["warmup"]:
                    samples.append(dt_ms)
        finally:
            if conn is not None:
                try:
                    conn.disconnect()
                except Exception:
                    pass
        if not samples:
            pytest.fail(f"{t['name']}: no ATR samples collected")
        ledger.record_samples(t["name"], t["transport"],
                              "atr_latency_ms", samples)
