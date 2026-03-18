#!/usr/bin/env python3
"""Soak Test 07: GPG Card Operations

Tests GnuPG's OpenPGP card interface:
- gpg --card-status
- gpg --card-edit operations
- Card data fetch
- Key listing

These exercises T=1 TPDU, command chaining, and PIN management through pcscd.
"""

import sys
import os
import subprocess
import time
import tempfile

sys.path.insert(0, os.path.dirname(__file__))
from soaklib import (
    discover_readers, connect_reader, disconnect_reader, restart_pcscd,
    stop_pcscd, run_test_on_both, save_results,
    SuiteResult, LOG_BASE,
)

SUITE_DIR = LOG_BASE / "soak-07-gpg-card-edit"
LOG_FILE = SUITE_DIR / "pcscd.log"
GNUPGHOME = os.path.expanduser("~/.gnupg-soak-test")


def _gpg_cmd(cmd: str, timeout: int = 30) -> dict:
    env = {**os.environ, "GNUPGHOME": GNUPGHOME}
    os.makedirs(GNUPGHOME, exist_ok=True)
    try:
        result = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=timeout, env=env)
        return {"rc": result.returncode, "stdout": result.stdout[:2000], "stderr": result.stderr[:1000]}
    except subprocess.TimeoutExpired:
        return {"rc": -1, "stdout": "", "stderr": "TIMEOUT"}


def test_gpg_card_status(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _gpg_cmd("gpg --card-status 2>&1")


def test_gpg_card_list(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _gpg_cmd("gpg --list-card-keys 2>&1")


def test_gpg_card_edit_admin(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    with tempfile.NamedTemporaryFile(mode='w', suffix='.txt', delete=False) as f:
        f.write("admin\nquit\n")
        cmd_file = f.name
    try:
        cmd = f"gpg --command-fd 1 --card-edit < {cmd_file} 2>&1"
        return _gpg_cmd(cmd, timeout=20)
    finally:
        os.unlink(cmd_file)


def test_gpg_card_edit_fetch(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    with tempfile.NamedTemporaryFile(mode='w', suffix='.txt', delete=False) as f:
        f.write("fetch\nquit\n")
        cmd_file = f.name
    try:
        cmd = f"gpg --command-fd 1 --card-edit < {cmd_file} 2>&1"
        return _gpg_cmd(cmd, timeout=30)
    finally:
        os.unlink(cmd_file)


def test_gpg_card_edit_list(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    with tempfile.NamedTemporaryFile(mode='w', suffix='.txt', delete=False) as f:
        f.write("list\nquit\n")
        cmd_file = f.name
    try:
        cmd = f"gpg --command-fd 1 --card-edit < {cmd_file} 2>&1"
        return _gpg_cmd(cmd, timeout=20)
    finally:
        os.unlink(cmd_file)


def test_gpg_export_card_key(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _gpg_cmd("gpg --export-ssh-key 2>&1", timeout=20)


def test_gpg_connect_agent(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _gpg_cmd("gpg-connect-agent 'SCD GETATTR AID' /bye 2>&1", timeout=15)


def test_gpg_card_edit_name(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    with tempfile.NamedTemporaryFile(mode='w', suffix='.txt', delete=False) as f:
        f.write("name\nTest\nquit\n")
        cmd_file = f.name
    try:
        cmd = f"gpg --pinentry-mode loopback --command-fd 1 --card-edit < {cmd_file} 2>&1"
        return _gpg_cmd(cmd, timeout=20)
    finally:
        os.unlink(cmd_file)


def test_gpg_card_edit_lang(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    with tempfile.NamedTemporaryFile(mode='w', suffix='.txt', delete=False) as f:
        f.write("lang\nen\nquit\n")
        cmd_file = f.name
    try:
        cmd = f"gpg --pinentry-mode loopback --command-fd 1 --card-edit < {cmd_file} 2>&1"
        return _gpg_cmd(cmd, timeout=20)
    finally:
        os.unlink(cmd_file)


def main():
    print("=" * 60)
    print("SOAK TEST 07: GPG Card Operations")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-07-gpg-card-edit", suite_num=7)

    print("\n[1/5] Starting pcscd with CCID capture...")
    pcscd_proc = restart_pcscd(debug_level=0x0007, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/5] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45)
    if len(readers) < 2:
        print(f"ERROR: Need both readers. Found: {list(readers.keys())}", file=sys.stderr)
        stop_pcscd()
        return 1

    print("[3/5] Running GPG card tests...")
    run_test_on_both(suite, "gpg_card_status", test_gpg_card_status, readers)
    run_test_on_both(suite, "gpg_card_list", test_gpg_card_list, readers)
    run_test_on_both(suite, "gpg_card_edit_admin", test_gpg_card_edit_admin, readers)
    run_test_on_both(suite, "gpg_card_edit_list", test_gpg_card_edit_list, readers)
    run_test_on_both(suite, "gpg_card_edit_fetch", test_gpg_card_edit_fetch, readers)
    run_test_on_both(suite, "gpg_card_edit_name", test_gpg_card_edit_name, readers)
    run_test_on_both(suite, "gpg_card_edit_lang", test_gpg_card_edit_lang, readers)
    run_test_on_both(suite, "gpg_connect_agent", test_gpg_connect_agent, readers)
    run_test_on_both(suite, "gpg_export_card_key", test_gpg_export_card_key, readers)

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
