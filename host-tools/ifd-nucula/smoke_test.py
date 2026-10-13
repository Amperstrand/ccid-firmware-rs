#!/usr/bin/env python3
"""pcscd smoke scenario for libifd-nucula (issue #157).

Phases (driven by argv[1]):
  probe   — wait for the reader, connect, ATR, transmit, escape 0xD0
  absent  — reader must STILL be listed; card must be ABSENT
  recover — card must come back; transmit must work again
Exit 0 = phase passed, 1 = failed (with diagnostics on stdout).
"""
import sys
import time

from smartcard.System import readers
from smartcard.Exceptions import (
    NoReadersException,
    SmartcardException,
    CardConnectionException,
)

G05_ATR = "3B858131FE458073C8211084"
READER = "Nucula Mock (wifi IFD)"


def find_reader():
    try:
        rs = readers()
    except NoReadersException:
        return None
    for r in rs:
        if READER in str(r):
            return r
    return None


def wait_reader(timeout=10):
    dl = time.time() + timeout
    while time.time() < dl:
        r = find_reader()
        if r:
            return r
        time.sleep(0.4)
    return None


def card_state(r):
    """(present, atr-hex) via a short-lived status connection."""
    try:
        c = r.createConnection()
        c.connect()
        atr = " ".join(f"{b:02X}" for b in c.getATR())
        c.disconnect()
        return True, atr
    except CardConnectionException:
        return False, None
    except SmartcardException:
        return False, None


def phase_probe():
    r = wait_reader()
    if not r:
        print("FAIL: reader not listed within 10s")
        return 1
    print(f"reader listed: {r}")
    present, atr = card_state(r)
    if not present:
        print("FAIL: card not present on connect")
        return 1
    if atr.replace(" ", "") != G05_ATR:
        print(f"FAIL: ATR {atr} != expected {G05_ATR}")
        return 1
    print(f"ATR ok: {atr}")
    c = r.createConnection()
    c.connect()
    sel = [0x00, 0xA4, 0x04, 0x00]
    _, sw1, sw2 = c.transmit(sel)
    if (sw1, sw2) != (0x90, 0x00):
        print(f"FAIL: SELECT SW {sw1:02X} {sw2:02X}")
        return 1
    print("SELECT SW 90 00")
    # escape passthrough via SCardControl: query features, use the
    # FEATURE_CCID_ESC_COMMAND ioctl, send 0xD0 (mock answers 28 bytes)
    feats = bytes(c.control(0x42000000 + 3400, []))
    esc_ioctl = None
    for i in range(0, len(feats) - 5, 6):
        if feats[i] == 0x13:
            esc_ioctl = int.from_bytes(feats[i + 2:i + 6], "big")
    if esc_ioctl is None:
        print("FAIL: FEATURE_CCID_ESC_COMMAND not advertised")
        return 1
    diag = bytes(c.control(esc_ioctl, [0xD0]))
    if len(diag) != 28:
        print(f"FAIL: escape 0xD0 returned {len(diag)} bytes, expected 28")
        return 1
    print(f"escape 0xD0 passthrough ok ({len(diag)} bytes)")
    c.disconnect()
    return 0


def phase_absent():
    r = wait_reader()
    if not r:
        print("FAIL: reader DISAPPEARED from pcscd (must stay listed)")
        return 1
    print("reader still listed after board death")
    dl = time.time() + 15
    while time.time() < dl:
        present, _ = card_state(r)
        if not present:
            print("card absent (presence poll caught the dead board)")
            return 0
        time.sleep(0.5)
    print("FAIL: card still present 15s after board death")
    return 1


def phase_recover():
    r = wait_reader()
    if not r:
        print("FAIL: reader gone")
        return 1
    dl = time.time() + 15
    while time.time() < dl:
        present, _ = card_state(r)
        if present:
            c = r.createConnection()
            c.connect()
            _, sw1, sw2 = c.transmit([0x00, 0xA4, 0x04, 0x00])
            c.disconnect()
            if (sw1, sw2) == (0x90, 0x00):
                print("recovered: card back + SELECT 90 00")
                return 0
            print(f"FAIL: recovered but SELECT SW {sw1:02X} {sw2:02X}")
            return 1
        time.sleep(0.5)
    print("FAIL: card did not come back within 15s")
    return 1


def main():
    phase = sys.argv[1] if len(sys.argv) > 1 else "probe"
    return {"probe": phase_probe, "absent": phase_absent,
            "recover": phase_recover}[phase]()


if __name__ == "__main__":
    sys.exit(main())
