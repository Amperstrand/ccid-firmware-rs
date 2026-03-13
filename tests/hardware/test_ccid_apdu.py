#!/usr/bin/env python3
"""
Minimal end-to-end test for ccid-reader via pcscd.
Connect to the emulated IDBridge CT30-compatible reader (default VID:PID 08E6:3437),
get ATR, send one short APDU (e.g. SELECT or GET STATUS).
Requires: pcscd running, CCID driver, and the reader plugged in.
Usage:
  python3 test_ccid_apdu.py
  python3 test_ccid_apdu.py "IDBridge"
"""
import sys

try:
    from smartcard.System import readers
    from smartcard.scard import SCARD_PROTOCOL_T0, SCARD_PROTOCOL_T1
except ImportError:
    print("Install pyscard: pip install pyscard", file=sys.stderr)
    sys.exit(1)


def main():
    rlist = readers()
    if not rlist:
        print("No readers found. Is pcscd running? Is the CCID reader plugged in?")
        sys.exit(1)

    # Optional: filter by reader name (e.g. "IDBridge", "Gemalto", or "PC Twin")
    name_filter = sys.argv[1] if len(sys.argv) > 1 else None
    if name_filter:
        rlist = [r for r in rlist if name_filter in str(r)]
        if not rlist:
            print(f"No reader matching '{name_filter}'. Available: {[str(r) for r in readers()]}")
            sys.exit(1)

    reader = rlist[0]
    print(f"Using reader: {reader}")

    try:
        conn = reader.createConnection()
        conn.connect(SCARD_PROTOCOL_T0 | SCARD_PROTOCOL_T1)
    except Exception as e:
        print(f"Connect failed (insert a card?): {e}")
        sys.exit(1)

    try:
        atr = bytes(conn.getATR())
        print(f"ATR ({len(atr)} bytes): {atr.hex().upper()}")
        if atr and atr[0] == 0x3B:
            print("  TS=3B (direct convention)")
        elif atr and atr[0] == 0x3F:
            print("  TS=3F (inverse convention)")
    except Exception as e:
        print(f"Get ATR failed: {e}")
        sys.exit(1)

    # Send a short APDU: SELECT by AID (e.g. minimal 00 A4 04 00 00 = SELECT file by path, no data)
    # Or GET STATUS style: 80 F2 00 00 00 (depends on card). Use a neutral SELECT.
    apdu = bytes([0x00, 0xA4, 0x04, 0x00, 0x00])  # SELECT file, no data, Le=0
    print(f"APDU: {apdu.hex().upper()}")
    try:
        data, sw1, sw2 = conn.transmit(list(apdu))
        resp = bytes(data) if data else b""
        print(f"Response: data={resp.hex().upper() or '(none)'} SW1={sw1:02X} SW2={sw2:02X}")
        if (sw1, sw2) == (0x90, 0x00):
            print("  SW=9000 (success)")
        else:
            print(f"  SW={sw1:02X}{sw2:02X}")
    except Exception as e:
        print(f"Transmit failed: {e}")
        sys.exit(1)

    print("OK: ATR and one APDU completed.")


if __name__ == "__main__":
    main()
