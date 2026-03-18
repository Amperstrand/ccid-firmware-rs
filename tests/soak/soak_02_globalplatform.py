#!/usr/bin/env python3
"""Soak Test 02: GlobalPlatform Operations

Uses gp.jar to:
- List installed applets on both cards
- Install a HelloWorld-style applet
- Delete the applet
- Install PivApplet
- Test GP card manager operations (GET STATUS, etc.)

This exercises extended APDUs, large payloads, and SCP secure channel.
"""

import sys
import os
import subprocess
import time
import re

sys.path.insert(0, os.path.dirname(__file__))
from soaklib import (
    discover_readers, connect_reader, disconnect_reader, restart_pcscd,
    stop_pcscd, run_test_on_both, save_results,
    SuiteResult, LOG_BASE,
)

SUITE_DIR = LOG_BASE / "soak-02-globalplatform"
LOG_FILE = SUITE_DIR / "pcscd.log"
GP_JAR = "/tmp/gp.jar"
PIV_CAP = "/tmp/caps/PivApplet.cap"

AUTH_KEYS = "--key-enc 5A9E63D03BADBC2A240FE8F534709EDF --key-mac 7CCC1E79D64FC5FA263B8F2955282998 --key-dek B040703EC3DE23EE8AE4CFB6D632AA80"
DEFAULT_KEYS = "--key-enc 404142434445464748494A4B4C4D4E4F --key-mac 404142434445464748494A4B4C4D4E4F --key-dek 404142434445464748494A4B4C4D4E4F"


def normalize_gp_output(s: str) -> str:
    lines = s.split("\n")
    cleaned = []
    for line in lines:
        if re.search(r"BCF852F0|CT30-001|--reader|--key-", line):
            continue
        cleaned.append(line)
    return "\n".join(cleaned)


def gp_command(reader_name: str, subcommand: str, extra_args: str = "", timeout: int = 60) -> dict:
    keys = AUTH_KEYS if "BCF852F0" in reader_name else DEFAULT_KEYS
    cmd = (
        f"sudo java -jar {GP_JAR} "
        f"--reader \"{reader_name}\" "
        f"{keys} "
        f"{subcommand} {extra_args}"
    )
    try:
        result = subprocess.run(
            cmd, shell=True, capture_output=True, text=True, timeout=timeout
        )
        return {
            "returncode": result.returncode,
            "stdout": result.stdout[:2000],
            "stderr": result.stderr[:1000],
        }
    except subprocess.TimeoutExpired:
        return {"returncode": -1, "stdout": "", "stderr": "TIMEOUT"}
    except Exception as e:
        return {"returncode": -2, "stdout": "", "stderr": str(e)}


def test_gp_list(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    r = gp_command(reader_info["name"], "--list")
    if r["returncode"] != 0:
        raise RuntimeError(f"gp --list failed (rc={r['returncode']}): {r['stderr']}")
    return {"applets": r["stdout"].strip()[:1000]}


def test_gp_info(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    r = gp_command(reader_info["name"], "--info")
    if r["returncode"] != 0:
        raise RuntimeError(f"gp --info failed (rc={r['returncode']}): {r['stderr']}")
    return {"info": r["stdout"][:500]}


def test_gp_status(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    r = gp_command(reader_info["name"], "--status")
    return {"status": r["stdout"][:500], "rc": r["returncode"]}


def test_gp_get_data(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    r = gp_command(reader_info["name"], "--get-data 00E0", timeout=30)
    return {"data": r["stdout"][:500], "rc": r["returncode"]}


def test_gp_install_piv(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    if not os.path.exists(PIV_CAP):
        return {"skipped": True, "reason": "PIV CAP file not found"}
    r = gp_command(reader_info["name"], f"--install {PIV_CAP}", timeout=120)
    return {"result": r["stdout"][:500], "rc": r["returncode"]}


def test_gp_uninstall_piv(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    r = gp_command(
        reader_info["name"],
        "--delete A00000030800001000",
        timeout=60,
    )
    return {"result": r["stdout"][:500], "rc": r["returncode"]}


def main():
    print("=" * 60)
    print("SOAK TEST 02: GlobalPlatform Operations")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-02-globalplatform", suite_num=2)

    if not os.path.exists(GP_JAR):
        print("ERROR: gp.jar not found at /tmp/gp.jar", file=sys.stderr)
        return 1

    print("\n[1/5] Starting pcscd with CCID capture...")
    pcscd_proc = restart_pcscd(debug_level=0x0007, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/5] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45)
    if len(readers) < 2:
        print(f"ERROR: Need both readers. Found: {list(readers.keys())}", file=sys.stderr)
        stop_pcscd()
        return 1

    print("[3/5] Running GlobalPlatform tests...")
    run_test_on_both(suite, "gp_list", test_gp_list, readers, normalize=normalize_gp_output, expect_response_diff=True)
    run_test_on_both(suite, "gp_info", test_gp_info, readers, normalize=normalize_gp_output)
    run_test_on_both(suite, "gp_status", test_gp_status, readers, normalize=normalize_gp_output)
    run_test_on_both(suite, "gp_get_data", test_gp_get_data, readers)

    if os.path.exists(PIV_CAP):
        print("  [PIV] Installing PivApplet on both cards...")
        run_test_on_both(suite, "gp_install_piv", test_gp_install_piv, readers, normalize=normalize_gp_output)
        print("  [PIV] Listing after install...")
        run_test_on_both(suite, "gp_list_after_install", test_gp_list, readers, normalize=normalize_gp_output, expect_response_diff=True)
        print("  [PIV] Uninstalling PivApplet...")
        run_test_on_both(suite, "gp_uninstall_piv", test_gp_uninstall_piv, readers, normalize=normalize_gp_output)
        print("  [PIV] Listing after uninstall...")
        run_test_on_both(suite, "gp_list_after_uninstall", test_gp_list, readers, normalize=normalize_gp_output, expect_response_diff=True)
    else:
        print("  [PIV] Skipped - no CAP file")

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
