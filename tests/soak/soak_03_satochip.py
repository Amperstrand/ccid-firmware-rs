#!/usr/bin/env python3
"""Soak Test 03: SatoChip Operations

Uses pysatochip to:
- Connect and establish secure channel
- Verify PIN
- Get card serial
- BIP32 public key derivation
- Sign a test message

Exercises extended APDUs, ECDSA, custom CLA/INS.
"""

import sys
import os
import time

sys.path.insert(0, os.path.dirname(__file__))
from soaklib import (
    discover_readers, connect_reader, disconnect_reader, restart_pcscd,
    stop_pcscd, run_test_on_both, save_results,
    SuiteResult, LOG_BASE,
)

SUITE_DIR = LOG_BASE / "soak-03-satochip"
LOG_FILE = SUITE_DIR / "pcscd.log"

try:
    from satochip.CardConnector import CardConnector
    from satochip.Pysatochip import Pysatochip
    from satochip.cmd import CMD
    SATOCHIP_AVAILABLE = True
except ImportError:
    SATOCHIP_AVAILABLE = False
    print("WARNING: pysatochip not importable, will use raw APDUs")


SATOCHIP_AID = bytes.fromhex("5F7361746F4368697000")

PIN_TEST = "1234"


def test_select_satochip_aid(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    SELECT = bytes.fromhex("00A4040008") + SATOCHIP_AID
    data, sw = conn.transmit(list(SELECT))
    return {"sw": f"0x{(data[-2] << 8) | data[-1]:04X}", "data": bytes(data[:-2]).hex() if len(data) > 2 else ""}


def test_satochip_get_serial(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    conn.disconnect()
    time.sleep(0.3)
    conn = connect_reader(reader_info)

    if SATOCHIP_AVAILABLE:
        try:
            cc = CardConnector()
            cc.card_select(conn)
            pysc = Pysatochip(cc)
            pysc.init()
            serial = pysc.get_serial()
            return {"serial": str(serial)}
        except Exception as e:
            raise RuntimeError(f"SatoChip get_serial failed: {e}")

    GET_SERIAL_APDU = bytes.fromhex("80F8000000")
    data, sw1, sw2 = conn.transmit(list(GET_SERIAL_APDU))
    return {"sw": f"0x{(sw1 << 8) | sw2:04X}", "data": bytes(data).hex()}


def test_satochip_verify_pin(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    conn.disconnect()
    time.sleep(0.3)
    conn = connect_reader(reader_info)

    if SATOCHIP_AVAILABLE:
        try:
            cc = CardConnector()
            cc.card_select(conn)
            pysc = Pysatochip(cc)
            pysc.init()
            pysc.verify_pin(PIN_TEST.encode())
            return {"status": "pin_verified"}
        except Exception as e:
            err_str = str(e)
            if "6982" in err_str or "6983" in err_str:
                return {"status": "pin_locked_or_wrong", "error": err_str}
            raise RuntimeError(f"SatoChip verify_pin failed: {e}")

    VERIFY = bytes.fromhex("80F2000208") + PIN_TEST.encode().ljust(8, b"\x00")
    data, sw1, sw2 = conn.transmit(list(VERIFY))
    return {"sw": f"0x{(sw1 << 8) | sw2:04X}", "data": bytes(data).hex()}


def test_satochip_bip32_pubkey(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    conn.disconnect()
    time.sleep(0.3)
    conn = connect_reader(reader_info)

    if SATOCHIP_AVAILABLE:
        try:
            cc = CardConnector()
            cc.card_select(conn)
            pysc = Pysatochip(cc)
            pysc.init()
            pysc.verify_pin(PIN_TEST.encode())
            pubkey = pysc.get_bip32_extendedkey("m/44'/0'/0'/0/0")
            return {"pubkey_len": len(str(pubkey)), "pubkey_prefix": str(pubkey)[:100]}
        except Exception as e:
            err_str = str(e)
            if "6982" in err_str:
                return {"status": "pin_required", "error": err_str}
            raise RuntimeError(f"SatoChip bip32 failed: {e}")

    BIP32 = bytes.fromhex("80F4020035") + b"\x00" * 4 + b"\x80\x00\x00\x2c" + b"\x00" * 21 + b"\x00" * 20 + b"\x00"
    data, sw1, sw2 = conn.transmit(list(BIP32))
    return {"sw": f"0x{(sw1 << 8) | sw2:04X}", "data": bytes(data).hex()[:200]}


def test_satochip_sign_message(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    conn.disconnect()
    time.sleep(0.3)
    conn = connect_reader(reader_info)

    if SATOCHIP_AVAILABLE:
        try:
            cc = CardConnector()
            cc.card_select(conn)
            pysc = Pysatochip(cc)
            pysc.init()
            pysc.verify_pin(PIN_TEST.encode())
            import hashlib
            msg_hash = hashlib.sha256(b"test message for soak test").digest()
            sig = pysc.sign_message(msg_hash, "m/44'/0'/0'/0/0")
            return {"sig_len": len(sig) if sig else 0}
        except Exception as e:
            err_str = str(e)
            if "6982" in err_str:
                return {"status": "pin_required", "error": err_str}
            raise RuntimeError(f"SatoChip sign failed: {e}")

    return {"status": "skipped_no_pysatochip"}


def main():
    print("=" * 60)
    print("SOAK TEST 03: SatoChip Operations")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-03-satochip", suite_num=3)

    print("\n[1/5] Starting pcscd with CCID capture...")
    pcscd_proc = restart_pcscd(debug_level=0x0007, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/5] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45)
    if len(readers) < 2:
        print(f"ERROR: Need both readers. Found: {list(readers.keys())}", file=sys.stderr)
        stop_pcscd()
        return 1

    print("[3/5] Running SatoChip tests...")
    run_test_on_both(suite, "select_satochip_aid", test_select_satochip_aid, readers)
    run_test_on_both(suite, "satochip_get_serial", test_satochip_get_serial, readers)
    run_test_on_both(suite, "satochip_verify_pin", test_satochip_verify_pin, readers)
    run_test_on_both(suite, "satochip_bip32_pubkey", test_satochip_bip32_pubkey, readers)
    run_test_on_both(suite, "satochip_sign_message", test_satochip_sign_message, readers)

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
