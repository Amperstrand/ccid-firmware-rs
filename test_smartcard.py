#!/usr/bin/env python3
"""
Generic smartcard test for CCID readers via pcscd.
Tests basic CCID functionality: reader enumeration, card detection, ATR, and APDU exchange.

Usage:
  python3 test_smartcard.py                    # use first available reader
  python3 test_smartcard.py IDBridge           # use reader matching "IDBridge"
  python3 test_smartcard.py --try-all          # try all readers until one has a card
"""
import sys

try:
    from smartcard.System import readers
    from smartcard.scard import SCARD_PROTOCOL_T0, SCARD_PROTOCOL_T1
except ImportError:
    print("Install pyscard: pip install pyscard", file=sys.stderr)
    sys.exit(1)


def test_reader(reader, verbose=True):
    """Test a single reader: connect, get ATR, send basic APDU."""
    if verbose:
        print(f"\nTesting reader: {reader}")
    
    try:
        conn = reader.createConnection()
        conn.connect(SCARD_PROTOCOL_T0 | SCARD_PROTOCOL_T1)
    except Exception as e:
        if verbose:
            print(f"  Connect failed: {e}")
        return False
    
    # Get ATR
    try:
        atr = bytes(conn.getATR())
        if verbose:
            print(f"  ATR ({len(atr)} bytes): {atr.hex().upper()}")
            if atr and atr[0] == 0x3B:
                print("  Convention: Direct (TS=3B)")
            elif atr and atr[0] == 0x3F:
                print("  Convention: Inverse (TS=3F)")
    except Exception as e:
        if verbose:
            print(f"  Get ATR failed: {e}")
        return False
    
    # Send generic SELECT APDU (no data, just tests the path)
    apdu = bytes([0x00, 0xA4, 0x04, 0x00, 0x00])  # SELECT by AID, no data
    if verbose:
        print(f"  APDU: {apdu.hex().upper()}")
    
    try:
        data, sw1, sw2 = conn.transmit(list(apdu))
        resp = bytes(data) if data else b""
        if verbose:
            print(f"  Response: SW={sw1:02X}{sw2:02X}")
    except Exception as e:
        if verbose:
            print(f"  Transmit failed: {e}")
        return False
    
    if verbose:
        print("  OK: Reader functional")
    return True


def main():
    rlist = readers()
    if not rlist:
        print("No readers found. Is pcscd running? Is a CCID reader connected?")
        sys.exit(1)
    
    print(f"Found {len(rlist)} reader(s):")
    for i, r in enumerate(rlist):
        print(f"  [{i}] {r}")
    
    # Check for --try-all flag
    try_all = "--try-all" in sys.argv
    if try_all:
        sys.argv.remove("--try-all")
    
    # Optional name filter
    name_filter = sys.argv[1] if len(sys.argv) > 1 else None
    
    if name_filter:
        filtered = [r for r in rlist if name_filter.lower() in str(r).lower()]
        if not filtered:
            print(f"No reader matching '{name_filter}'")
            sys.exit(1)
        rlist = filtered
    
    if try_all:
        # Try each reader until one works
        for reader in rlist:
            if test_reader(reader):
                print(f"\nSuccess with: {reader}")
                sys.exit(0)
        print("\nNo reader with card found")
        sys.exit(1)
    else:
        # Test first matching reader
        reader = rlist[0]
        if test_reader(reader):
            sys.exit(0)
        else:
            sys.exit(1)


if __name__ == "__main__":
    main()
