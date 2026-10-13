"""Per-APDU round-trip latency: fixed SELECT + GET DATA (read-only).

Metrics ``apdu_rtt_select_ms`` / ``apdu_rtt_get_data_ms``: per-APDU
wall time over one long-lived connection, warmup samples discarded,
timed with time.perf_counter under disabled GC (perflib.py header;
README.md has the full methodology + citations).

APDU order matters: GET DATA tag 004F is only meaningful inside the
selected OpenPGP applet, so SELECT runs first on the same connection.
On a card without the applet both probes still measure transport RTT
(the card answers 6A82/6A88-ish SW) — every completed transmit is a
sample; the SW distribution is printed for the record but not asserted,
because the metric is transport latency, not applet semantics (and vpcd
legitimately answers 64 00 on transient RF drops — runbook "Wire
format").

Error policy: 3 consecutive transmit exceptions stop that reader's
stream (whatever completed is recorded); zero samples for a probe is a
failure.
"""

import time

import pytest

from .perflib import RTT_APDUS, connect_reader_obj, no_gc, transmit

#: consecutive transmit failures before abandoning a reader's sample set
ERROR_STRIKES = 3


def test_apdu_rtt(reader_target, ledger):
    for t in reader_target:
        p = t["params"]
        print(f"\n[perf] APDU RTT: {t['name']} "
              f"(warmup={p['warmup']} + {p['iterations']} iters/probe)")
        conn, _ = connect_reader_obj(t["reader_obj"])
        try:
            for label, apdu in RTT_APDUS:
                samples, sw_counts, strikes = [], {}, 0
                for i in range(p["warmup"] + p["iterations"]):
                    try:
                        with no_gc():
                            t0 = time.perf_counter()
                            _resp, sw = transmit(conn, apdu)
                            dt_ms = (time.perf_counter() - t0) * 1000.0
                    except Exception as e:  # transport-level failure
                        strikes += 1
                        print(f"  [perf] {t['name']} transmit error "
                              f"({strikes}/{ERROR_STRIKES}): {str(e)[:80]}")
                        if strikes >= ERROR_STRIKES:
                            break
                        continue
                    strikes = 0
                    sw_key = f"{sw:04X}"
                    sw_counts[sw_key] = sw_counts.get(sw_key, 0) + 1
                    if i >= p["warmup"]:
                        samples.append(dt_ms)
                print(f"  [perf] {t['name']} {label}: SW distribution {sw_counts}")
                if not samples:
                    pytest.fail(f"{t['name']}: no RTT samples for the "
                                f"{label} APDU")
                ledger.record_samples(t["name"], t["transport"],
                                      f"apdu_rtt_{label}_ms", samples)
        finally:
            try:
                conn.disconnect()
            except Exception:
                pass
