#!/usr/bin/env python3

import sys

try:
    from smartcard.System import readers
    from smartcard.scard import SCARD_PROTOCOL_T0, SCARD_PROTOCOL_T1
except ImportError:
    print("Install pyscard: pip install pyscard", file=sys.stderr)
    sys.exit(1)


SEEDKEEPER_AID = bytes([0x53, 0x65, 0x65, 0x64, 0x4B, 0x65, 0x65, 0x70, 0x65, 0x72])


def apdu_select_seedkeeper():
    return bytes([0x00, 0xA4, 0x04, 0x00, len(SEEDKEEPER_AID)]) + SEEDKEEPER_AID


def apdu_get_status_like():
    return bytes([0xB0, 0xA7, 0x00, 0x00])


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

    sel = apdu_select_seedkeeper()
    data, sw1, sw2 = conn.transmit(list(sel))
    print(f"SELECT SeedKeeper SW={sw1:02X}{sw2:02X}")
    if (sw1, sw2) != (0x90, 0x00):
        return 2

    apdu = apdu_get_status_like()
    data, sw1, sw2 = conn.transmit(list(apdu))
    print(f"GET_STATUS-like SW={sw1:02X}{sw2:02X} data_len={len(data)}")
    if (sw1, sw2) not in {(0x90, 0x00), (0x9C, 0x20), (0x9C, 0x06)}:
        return 3

    print("Non-destructive SeedKeeper smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
