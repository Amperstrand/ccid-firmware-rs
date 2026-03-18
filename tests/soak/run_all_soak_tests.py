#!/usr/bin/env python3
"""CCID Firmware Soak Test Master Orchestrator

Runs all 10 test suites sequentially, collecting results and generating
a final report. Can be used standalone or invoked by Gas Town polecats.

Usage:
    python3 run_all_soak_tests.py [--suite NUM] [--skip NUM] [--list]
"""

import argparse
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

SCRIPT_DIR = Path(__file__).parent
LOG_BASE = Path("/home/ubuntu/gt/ccid_firmware/soak-test-logs")
RIG_DIR = Path("/home/ubuntu/gt/ccid_firmware/mayor/rig")
GITHUB_REPO = "Amperstrand/ccid-firmware-rs"

SUITES = [
    {"num": 1, "name": "smoke", "script": "soak_01_smoke.py", "desc": "Basic CCID operations"},
    {"num": 2, "name": "globalplatform", "script": "soak_02_globalplatform.py", "desc": "GP install/delete/list"},
    {"num": 3, "name": "satochip", "script": "soak_03_satochip.py", "desc": "SatoChip pysatochip"},
    {"num": 4, "name": "openpgp", "script": "soak_04_openpgp.py", "desc": "OpenPGP card ops"},
    {"num": 5, "name": "piv", "script": "soak_05_piv.py", "desc": "PIV operations"},
    {"num": 6, "name": "opensc-toolchain", "script": "soak_06_opensc.py", "desc": "OpenSC tools"},
    {"num": 7, "name": "gpg-card-edit", "script": "soak_07_gpg.py", "desc": "GPG card operations"},
    {"num": 8, "name": "extended-apdu", "script": "soak_08_extended_apdu.py", "desc": "Extended APDU stress"},
    {"num": 9, "name": "stress-repeat", "script": "soak_09_stress.py", "desc": "Rapid stress/repeat"},
    {"num": 10, "name": "cross-compare", "script": "soak_10_cross_compare.py", "desc": "Cross-reader CCID diff"},
]


def get_firmware_commit():
    try:
        result = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, cwd=str(RIG_DIR), timeout=5,
        )
        return result.stdout.strip()
    except Exception:
        return "unknown"


def run_suite(suite: dict) -> dict:
    script_path = SCRIPT_DIR / suite["script"]
    if not script_path.exists():
        return {"suite": suite["name"], "status": "SKIP", "reason": "Script not found"}

    print(f"\n{'=' * 60}")
    print(f"SUITE {suite['num']}/10: {suite['name']} — {suite['desc']}")
    print(f"Script: {script_path}")
    print(f"{'=' * 60}")

    start = time.time()
    try:
        result = subprocess.run(
            ["sudo", "python3", str(script_path)],
            capture_output=True, text=True, timeout=600,
            cwd=str(RIG_DIR),
        )
        elapsed = time.time() - start
        return {
            "suite": suite["name"],
            "num": suite["num"],
            "status": "PASS" if result.returncode == 0 else "FAIL",
            "rc": result.returncode,
            "elapsed_s": round(elapsed, 1),
            "stdout": result.stdout[-3000:] if result.stdout else "",
            "stderr": result.stderr[-1000:] if result.stderr else "",
        }
    except subprocess.TimeoutExpired:
        elapsed = time.time() - start
        return {
            "suite": suite["name"],
            "num": suite["num"],
            "status": "TIMEOUT",
            "elapsed_s": round(elapsed, 1),
            "error": "Suite exceeded 600s timeout",
        }
    except Exception as e:
        elapsed = time.time() - start
        return {
            "suite": suite["name"],
            "num": suite["num"],
            "status": "ERROR",
            "elapsed_s": round(elapsed, 1),
            "error": str(e),
        }


def generate_report(results: list):
    total_suites = len(results)
    passed = sum(1 for r in results if r["status"] == "PASS")
    failed = sum(1 for r in results if r["status"] == "FAIL")
    skipped = sum(1 for r in results if r["status"] in ("SKIP", "TIMEOUT", "ERROR"))
    total_elapsed = sum(r.get("elapsed_s", 0) for r in results)

    report_lines = [
        "=" * 60,
        "CCID FIRMWARE SOAK TEST REPORT",
        "=" * 60,
        f"Date:       {datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M:%S UTC')}",
        f"Commit:     {get_firmware_commit()}",
        f"Duration:   {total_elapsed:.0f}s ({total_elapsed/60:.1f}min)",
        f"Suites:     {passed}/{total_suites} passed, {failed} failed, {skipped} skipped",
        "",
        "SUITE RESULTS:",
        "-" * 40,
    ]

    for r in results:
        status_icon = {"PASS": "+", "FAIL": "!", "SKIP": "-", "TIMEOUT": "?", "ERROR": "X"}.get(r["status"], "?")
        elapsed = r.get("elapsed_s", 0)
        report_lines.append(f"  [{status_icon}] Suite {r.get('num', '?'):>2}: {r['suite']:<25} {r['status']:<8} ({elapsed:.0f}s)")

    report_lines.extend([
        "",
        "LOGS:",
        f"  {LOG_BASE}",
        "",
        "=" * 60,
    ])

    report_text = "\n".join(report_lines)

    report_path = LOG_BASE / "final-report.txt"
    report_path.write_text(report_text)

    json_path = LOG_BASE / "final-report.json"
    json_path.write_text(json.dumps({
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "firmware_commit": get_firmware_commit(),
        "total_suites": total_suites,
        "passed": passed,
        "failed": failed,
        "skipped": skipped,
        "total_elapsed_s": total_elapsed,
        "results": results,
    }, indent=2))

    return report_text, passed == total_suites


def main():
    parser = argparse.ArgumentParser(description="CCID Firmware Soak Test Orchestrator")
    parser.add_argument("--suite", type=int, help="Run only a specific suite (1-10)")
    parser.add_argument("--skip", type=int, action="append", help="Skip suite(s)")
    parser.add_argument("--list", action="store_true", help="List available suites")
    parser.add_argument("--no-stop-on-fail", action="store_true", help="Continue after failures")
    args = parser.parse_args()

    if args.list:
        for s in SUITES:
            print(f"  {s['num']:>2}. {s['name']:<25} {s['desc']}")
        return 0

    LOG_BASE.mkdir(parents=True, exist_ok=True)

    suites_to_run = SUITES
    if args.suite:
        suites_to_run = [s for s in SUITES if s["num"] == args.suite]
    if args.skip:
        suites_to_run = [s for s in suites_to_run if s["num"] not in args.skip]

    print("=" * 60)
    print("CCID FIRMWARE SOAK TEST")
    print(f"Commit: {get_firmware_commit()}")
    print(f"Suites: {len(suites_to_run)}")
    print(f"Logs:   {LOG_BASE}")
    print("=" * 60)

    results = []
    for suite in suites_to_run:
        result = run_suite(suite)
        results.append(result)
        print(f"\n  Result: {result['status']} ({result.get('elapsed_s', 0):.0f}s)")
        if result["status"] == "FAIL" and not args.no_stop_on_fail:
            print("\n  Stopping on first failure. Use --no-stop-on-fail to continue.")
            break
        time.sleep(2)

    report_text, all_passed = generate_report(results)
    print(f"\n{report_text}")

    return 0 if all_passed else 1


if __name__ == "__main__":
    sys.exit(main())
