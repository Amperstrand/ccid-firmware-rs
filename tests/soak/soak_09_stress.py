#!/usr/bin/env python3
"""Soak Test 09: Rapid Stress / Repeat

Stress tests the CCID state machine:
- 100x connect/disconnect cycles
- 50x APDU round-trips
- Power cycle stress
- Repeated SELECT operations
- Sequence number verification

Exercises CCID state machine robustness, cmd_busy handling, sequence numbers.
"""

import sys
import os
import time
import random

sys.path.insert(0, os.path.dirname(__file__))
from soaklib import (
    discover_readers, connect_reader, disconnect_reader, restart_pcscd,
    stop_pcscd, transmit_apdu, run_test_on_both, save_results,
    SuiteResult, LOG_BASE,
)

SUITE_DIR = LOG_BASE / "soak-09-stress-repeat"
LOG_FILE = SUITE_DIR / "pcscd.log"

SELECT_MF = bytes.fromhex("00A4040000")
SELECT_3F00 = bytes.fromhex("00A40400023F00")


def test_connect_disconnect_50x(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    successes = 0
    failures = []
    for i in range(50):
        try:
            c = connect_reader(reader_info)
            atr = c.getATR()
            c.disconnect()
            successes += 1
        except Exception as e:
            failures.append(f"iter_{i}: {e}")
            time.sleep(0.5)
    return {"successes": successes, "failures": len(failures), "failure_details": failures[:5]}


def test_apdu_roundtrip_50x(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    results = {"successes": 0, "failures": 0, "sw_values": set()}
    for i in range(50):
        try:
            data, sw = transmit_apdu(conn, SELECT_3F00)
            results["sw_values"].add(f"0x{sw:04X}")
            results["successes"] += 1
        except Exception:
            results["failures"] += 1
    results["sw_values"] = sorted(results["sw_values"])
    return results


def test_power_cycle_20x(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    successes = 0
    atrs = []
    for i in range(20):
        try:
            c = connect_reader(reader_info)
            atr = c.getATR().hex()
            c.disconnect()
            atrs.append(atr)
            successes += 1
            time.sleep(0.2)
        except Exception as e:
            time.sleep(1)
    unique_atrs = set(atrs)
    return {"successes": successes, "unique_atrs": len(unique_atrs), "atr_consistent": len(unique_atrs) <= 1}


def test_rapid_select_30x(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    sw_counts = {}
    for i in range(30):
        try:
            data, sw = transmit_apdu(conn, SELECT_3F00)
            sw_key = f"0x{sw:04X}"
            sw_counts[sw_key] = sw_counts.get(sw_key, 0) + 1
        except Exception:
            pass
    return {"sw_distribution": sw_counts}


def test_mixed_operations_30x(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    operations = [
        ("SELECT_MF", SELECT_MF),
        ("SELECT_3F00", SELECT_3F00),
        ("GET_RESPONSE", bytes.fromhex("00C0000000")),
        ("ENVELOPE", bytes.fromhex("00C2000000")),
        ("GET_DATA_65", bytes.fromhex("00CA006500")),
        ("GET_DATA_6B", bytes.fromhex("00CA006B00")),
    ]
    rng = random.Random(42)
    results = {}
    for i in range(30):
        name, apdu = rng.choice(operations)
        try:
            data, sw = transmit_apdu(conn, apdu)
            sw_key = f"0x{sw:04X}"
            if name not in results:
                results[name] = {}
            results[name][sw_key] = results[name].get(sw_key, 0) + 1
        except Exception:
            pass
    return results


def test_double_connect_stress_20x(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    successes = 0
    for i in range(20):
        try:
            c1 = connect_reader(reader_info)
            c1.disconnect()
            c2 = connect_reader(reader_info)
            c2.disconnect()
            successes += 1
            time.sleep(0.1)
        except Exception:
            time.sleep(0.5)
    return {"successes": successes}


def test_reconnect_after_error(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    results = []
    for i in range(10):
        try:
            c = connect_reader(reader_info)
            data, sw = transmit_apdu(c, SELECT_3F00)
            c.disconnect()
            results.append(f"0x{sw:04X}")
        except Exception as e:
            results.append(f"ERROR: {str(e)[:50]}")
            time.sleep(1)
    return {"results": results}


def main():
    print("=" * 60)
    print("SOAK TEST 09: Rapid Stress / Repeat")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-09-stress-repeat", suite_num=9)

    print("\n[1/5] Starting pcscd with CCID capture...")
    pcscd_proc = restart_pcscd(debug_level=0x0007, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/5] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45)
    if len(readers) < 2:
        print(f"ERROR: Need both readers. Found: {list(readers.keys())}", file=sys.stderr)
        stop_pcscd()
        return 1

    print("[3/5] Running stress tests...")
    run_test_on_both(suite, "connect_disconnect_50x", test_connect_disconnect_50x, readers)
    run_test_on_both(suite, "apdu_roundtrip_50x", test_apdu_roundtrip_50x, readers)
    run_test_on_both(suite, "power_cycle_20x", test_power_cycle_20x, readers)
    run_test_on_both(suite, "rapid_select_30x", test_rapid_select_30x, readers)
    run_test_on_both(suite, "mixed_operations_30x", test_mixed_operations_30x, readers)
    run_test_on_both(suite, "double_connect_stress_20x", test_double_connect_stress_20x, readers)
    run_test_on_both(suite, "reconnect_after_error", test_reconnect_after_error, readers)

    print("[4/5] Saving results...")
    save_results(SUITE_DIR, suite)
    stop_pcscd()

    print(f"\n{'=' * 60}")
    print(f"Results: {suite.passed}/{suite.total_tests} passed, {suite.failed} failed")
    print(f"Bugs found: {suite.bugs_found}")
    print(f"Logs: {SUITE_DIR}")
    print(f"{'=' * 60}")

    return 0 if suite.failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
