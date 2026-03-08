#!/usr/bin/env python3

import sys

try:
    from smartcard.System import readers
    from smartcard.scard import SCARD_PROTOCOL_T0, SCARD_PROTOCOL_T1
except ImportError:
    print("Install pyscard: pip install pyscard", file=sys.stderr)
    sys.exit(1)


def apdu_select_mf() -> bytes:
    return bytes([0x00, 0xA4, 0x00, 0x00, 0x02, 0x3F, 0x00])


def main() -> int:
    rlist = readers()
    if not rlist:
        print("No smartcard readers found")
        return 1

    reader = rlist[0]
    print(f"Using reader: {reader}")

    conn = reader.createConnection()
    conn.connect(SCARD_PROTOCOL_T0 | SCARD_PROTOCOL_T1)

    atr = bytes(conn.getATR())
    print(f"ATR ({len(atr)}): {atr.hex().upper()}")

    data, sw1, sw2 = conn.transmit(list(apdu_select_mf()))
    print(f"SELECT MF SW={sw1:02X}{sw2:02X} data_len={len(data)}")
    if (sw1, sw2) != (0x90, 0x00):
        return 2

    print("Non-destructive sysmocom SIM smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
