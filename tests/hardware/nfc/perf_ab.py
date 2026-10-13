#!/usr/bin/env python3
"""A/B performance harness for bench CCID readers (labgrid bench, host side).

Measures per-APDU round-trip latency through pcscd for every card-bearing
reader, resolves readers by IDENTITY (never index), and emits stats +
JSON for before/after comparisons.

Modes:
  default          one measurement pass across all readers (--n APDUs each)
  --soak MINUTES   sustained loop with drift/wedge detection
  --compare A B    diff two JSON result files

Usage:
  python3 perf_ab.py                       # baseline, 100 APDUs/reader
  python3 perf_ab.py --n 500 --json run.json
  python3 perf_ab.py --soak 10             # 10-minute soak
  python3 perf_ab.py --compare before.json after.json
"""
import argparse
import json
import statistics
import sys
import time

APDU = [0x00, 0xA4, 0x04, 0x00, 0x00]  # SELECT MF (short, card-answered)

# identity substrings -> label (doctrine: never enumeration order)
READER_FILTERS = [
    ("GemPCTwin", "m5stick-mfrc522"),
    ("Cherry", "stm32f469"),
    ("ACR1252 Dual Reader PICC", "acr1252-ref"),
    ("CardMan", "cardman-ref"),
]


def pct(sorted_vals, p):
    if not sorted_vals:
        return float("nan")
    k = min(int(len(sorted_vals) * p / 100), len(sorted_vals) - 1)
    return sorted_vals[k]


def measure_reader(reader, n_apdus, warmup=5):
    from smartcard.Exceptions import CardConnectionException, NoCardException
    result = {"errors": 0, "rtts": [], "sw": {}}
    try:
        conn = reader.createConnection()
        conn.connect()
    except (NoCardException, CardConnectionException) as e:
        result["connect_error"] = f"{type(e).__name__}: {e}"
        return result
    try:
        for i in range(warmup + n_apdus):
            t0 = time.perf_counter()
            try:
                data, sw1, sw2 = conn.transmit(APDU)
            except Exception as e:
                result["errors"] += 1
                result["last_error"] = f"{type(e).__name__}: {str(e)[:60]}"
                if result["errors"] >= 3:
                    break
                time.sleep(0.5)
                continue
            dt_ms = (time.perf_counter() - t0) * 1000.0
            if i >= warmup:
                result["rtts"].append(round(dt_ms, 3))
                key = f"{sw1:02X}{sw2:02X}"
                result["sw"][key] = result["sw"].get(key, 0) + 1
    finally:
        try:
            conn.disconnect()
        except Exception:
            pass
    return result


def summarize(label, r):
    if r.get("connect_error"):
        return f"{label:18s} CONNECT FAIL: {r['connect_error'][:50]}"
    rtts = sorted(r["rtts"])
    if not rtts:
        return f"{label:18s} no samples (errors={r['errors']})"
    mean = statistics.fmean(rtts)
    return (
        f"{label:18s} n={len(rtts):4d} err={r['errors']} "
        f"p50={pct(rtts,50):7.2f} p95={pct(rtts,95):7.2f} "
        f"max={rtts[-1]:7.2f} mean={mean:7.2f} ms  sw={dict(sorted(r['sw'].items(), key=lambda kv: -kv[1])[:2])}"
    )


def run_pass(n_apdus, json_path=None):
    from smartcard.System import readers
    all_rs = readers()
    report = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "n": n_apdus, "readers": {}}
    print(f"=== perf_ab pass: {n_apdus} APDUs/reader ({report['ts']}) ===")
    for filt, label in READER_FILTERS:
        matches = [r for r in all_rs if filt in str(r)]
        if not matches:
            print(f"{label:18s} reader not present")
            continue
        r = measure_reader(matches[0], n_apdus)
        print(summarize(label, r))
        report["readers"][label] = {k: v for k, v in r.items() if k != "rtts"} | {
            "rtt_stats": {
                "p50": pct(sorted(r["rtts"]), 50),
                "p95": pct(sorted(r["rtts"]), 95),
                "max": max(r["rtts"]) if r["rtts"] else None,
                "mean": statistics.fmean(r["rtts"]) if r["rtts"] else None,
            }
        }
    if json_path:
        with open(json_path, "w") as f:
            json.dump(report, f, indent=1)
        print(f"json: {json_path}")
    return report


def soak(minutes, n_apdus):
    from smartcard.System import readers
    t_end = time.time() + minutes * 60
    round_no = 0
    print(f"=== soak: {minutes} min, {n_apdus} APDUs/reader/round, drift+wedge watch ===")
    baselines = {}
    while time.time() < t_end:
        round_no += 1
        all_rs = readers()
        for filt, label in READER_FILTERS:
            matches = [r for r in all_rs if filt in str(r)]
            if not matches:
                continue
            r = measure_reader(matches[0], n_apdus, warmup=2)
            p50 = pct(sorted(r["rtts"]), 50) if r["rtts"] else None
            errs = r["errors"] + (1 if r.get("connect_error") else 0)
            base = baselines.setdefault(label, {"p50": p50, "errs": 0})
            base["errs"] += errs
            drift = ""
            if p50 is not None and base["p50"]:
                ratio = p50 / base["p50"] if base["p50"] else 0
                if ratio > 3:
                    drift = f"  <<< DRIFT x{ratio:.1f}"
            flag = f"  <<< ERRORS ({r.get('last_error','')[:40]})" if errs else ""
            print(
                f"[round {round_no:3d}] {label:18s} p50={p50 if p50 is None else round(p50,2)}ms "
                f"err={errs}{flag}{drift}"
            )
        time.sleep(min(60, max(5, t_end - time.time())))
    print("=== soak complete ===")


def compare(path_a, path_b):
    with open(path_a) as f:
        a = json.load(f)
    with open(path_b) as f:
        b = json.load(f)
    print(f"A: {a['ts']}  vs  B: {b['ts']}  (n={a['n']}/{b['n']})")
    print(f"{'reader':18s} {'A p50':>8s} {'B p50':>8s} {'delta':>8s} {'A p95':>8s} {'B p95':>8s}")
    for label in a["readers"]:
        ra, rb = a["readers"].get(label), b["readers"].get(label)
        if not ra or not rb:
            continue
        sa, sb = ra["rtt_stats"], rb["rtt_stats"]
        a50, b50 = sa["p50"], sb["p50"]
        delta = (b50 - a50) if (a50 is not None and b50 is not None) else float("nan")
        print(
            f"{label:18s} {a50:8.2f} {b50:8.2f} {delta:+8.2f} "
            f"{sa['p95']:8.2f} {sb['p95']:8.2f}"
        )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=100)
    ap.add_argument("--json", default=None)
    ap.add_argument("--soak", type=int, default=0, help="minutes")
    ap.add_argument("--compare", nargs=2, metavar=("A", "B"))
    args = ap.parse_args()
    if args.compare:
        compare(*args.compare)
        return
    if args.soak:
        soak(args.soak, args.n)
        return
    run_pass(args.n, args.json)


if __name__ == "__main__":
    sys.exit(main())
