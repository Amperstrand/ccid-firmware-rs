#!/usr/bin/env python3
"""
SeedKeeper test via pcscd: connect to reader, SELECT applet, VERIFY_PIN, GET_STATUS,
LIST_SECRETS, optionally export first secret. Uses standard pyscard (shared mode); PIN default 1234.
Usage:
  python3 test_seedkeeper.py                    # use first available reader
  python3 test_seedkeeper.py --try-all          # try every reader until one has a card
  python3 test_seedkeeper.py IDBridge 1234      # use reader matching IDBridge CT30
  python3 test_seedkeeper.py Gemalto 1234       # match Gemalto reader string
  python3 test_seedkeeper.py "PC Twin" 1234    # match libccid reader alias
If you see "Card is unpowered" (0x80100067): reader is seen but IccPowerOn/ATR may be failing
on the firmware side; capture defmt logs during connect to debug.
"""
import sys

try:
    from smartcard.System import readers
    from smartcard.scard import SCARD_PROTOCOL_T0, SCARD_PROTOCOL_T1
except ImportError:
    print("Install pyscard: pip install pyscard", file=sys.stderr)
    sys.exit(1)

# SeedKeeper AID
SEEDKEEPER_AID = bytes([0x53, 0x65, 0x65, 0x64, 0x4B, 0x65, 0x65, 0x70, 0x65, 0x72])
# APDU helpers: CLA=B0 for SeedKeeper
def select_seedkeeper():
    return bytes([0x00, 0xA4, 0x04, 0x00, len(SEEDKEEPER_AID)]) + SEEDKEEPER_AID
def verify_pin(pin_ascii):
    pin = pin_ascii.encode("ascii") if isinstance(pin_ascii, str) else pin_ascii
    return bytes([0xB0, 0x42, 0x00, 0x00, len(pin)]) + pin
def get_status():
    return bytes([0xB0, 0xA7, 0x00, 0x00])
def list_secrets_init():
    return bytes([0xB0, 0xA6, 0x00, 0x01])
def list_secrets_next():
    return bytes([0xB0, 0xA6, 0x00, 0x02])
def export_secret_init(secret_id):
    id_hi, id_lo = (secret_id >> 8) & 0xFF, secret_id & 0xFF
    return bytes([0xB0, 0xA2, 0x01, 0x01, 0x02, id_hi, id_lo])
def export_secret_update(secret_id):
    id_hi, id_lo = (secret_id >> 8) & 0xFF, secret_id & 0xFF
    return bytes([0xB0, 0xA2, 0x01, 0x02, 0x02, id_hi, id_lo])

def parse_get_status(data):
    if len(data) >= 6:
        return (data[0] << 8) | data[1], (data[2] << 8) | data[3], (data[4] << 8) | data[5]
    return 0, 0, 0

def parse_secret_header(data):
    if len(data) < 14:
        return None
    secret_id = (data[0] << 8) | data[1]
    secret_type = data[2]
    label_len = data[13]
    label = data[14:14 + label_len].decode("utf-8", errors="replace") if label_len and len(data) > 14 else ""
    return {"id": secret_id, "type": secret_type, "label": label}

def main():
    argv = [a for a in sys.argv[1:] if not a.startswith("--")]
    try_all = "--try-all" in sys.argv
    reader_filter = argv[0] if len(argv) > 0 else None
    pin = argv[1] if len(argv) > 1 else "1234"

    rlist = readers()
    if not rlist:
        print("No readers found.")
        sys.exit(1)

    # Name patterns that match our emulated IDBridge CT30 / PC Twin reader
    ccid_patterns = ("IDBridge", "CT30", "Gemalto", "PC Twin", "08E6", "3437")
    if try_all:
        # Prefer reader that looks like our CCID; then use first reader that has a card
        for pattern in ccid_patterns:
            rlist = [r for r in readers() if pattern in str(r)]
            if rlist:
                break
        if not rlist:
            rlist = list(readers())
        print(f"Trying {len(rlist)} reader(s): {[str(r) for r in rlist]}")
    elif reader_filter:
        rlist = [r for r in rlist if reader_filter in str(r)]
        if not rlist:
            print(f"No reader matching '{reader_filter}'. Available: {[str(r) for r in readers()]}")
            sys.exit(1)
    reader = None
    conn = None
    for r in rlist:
        try:
            c = r.createConnection()
            c.connect(SCARD_PROTOCOL_T0 | SCARD_PROTOCOL_T1)
            reader = r
            conn = c
            break
        except Exception as e:
            if try_all:
                print(f"  {r}: {e}")
            else:
                print(f"Connect failed: {e}")
                sys.exit(1)
    if not reader or not conn:
        print("No reader had a card inserted.")
        sys.exit(1)
    print(f"Using reader: {reader}")
    print(f"PIN: {pin}")

    def transmit(apdu):
        data, sw1, sw2 = conn.transmit(list(apdu))
        return bytes(data) if data else b"", sw1, sw2

    try:
        atr = bytes(conn.getATR())
        print(f"ATR: {atr.hex().upper()} ({len(atr)} bytes)")
    except Exception as e:
        print(f"Get ATR failed: {e}")
        sys.exit(1)

    # SELECT SeedKeeper
    print("\n[1] SELECT SeedKeeper...")
    data, sw1, sw2 = transmit(select_seedkeeper())
    if (sw1, sw2) != (0x90, 0x00):
        print(f"  SELECT failed: SW={sw1:02X}{sw2:02X}")
        sys.exit(1)
    print("  OK")

    # SW 9C20 = secure channel required; host must use pysatochip for full SeedKeeper flow (this script sends raw APDUs without secure channel).
    # VERIFY_PIN
    print("\n[2] VERIFY_PIN...")
    data, sw1, sw2 = transmit(verify_pin(pin))
    if sw1 == 0x63:
        print(f"  PIN failed, attempts left: {sw2 & 0x0F}")
        sys.exit(1)
    if (sw1, sw2) != (0x90, 0x00):
        print(f"  VERIFY_PIN failed: SW={sw1:02X}{sw2:02X}")
        sys.exit(1)
    print("  OK")

    # GET_STATUS
    print("\n[3] GET_STATUS...")
    data, sw1, sw2 = transmit(get_status())
    if (sw1, sw2) == (0x90, 0x00):
        nb, total, free = parse_get_status(data)
        print(f"  nb_secrets={nb}, total_memory={total}, free_memory={free}")
    else:
        print(f"  SW={sw1:02X}{sw2:02X}")

    # LIST_SECRETS
    print("\n[4] LIST_SECRETS...")
    secrets = []
    data, sw1, sw2 = transmit(list_secrets_init())
    if (sw1, sw2) == (0x90, 0x00):
        h = parse_secret_header(data)
        if h:
            secrets.append(h)
            print(f"  Secret 1: id={h['id']} type=0x{h['type']:02X} label={h['label']!r}")
    while True:
        data, sw1, sw2 = transmit(list_secrets_next())
        if (sw1, sw2) == (0x9C, 0x12):
            break
        if (sw1, sw2) == (0x90, 0x00):
            h = parse_secret_header(data)
            if h:
                secrets.append(h)
                print(f"  Secret {len(secrets)}: id={h['id']} type=0x{h['type']:02X} label={h['label']!r}")
        else:
            break

    print(f"\nTotal secrets: {len(secrets)}")

    if not secrets:
        print("No secrets to export.")
        return

    # Export first secret (or first BIP39/Masterseed)
    target = None
    for s in secrets:
        if s["type"] in (0x30, 0x10):
            target = s
            break
    if not target:
        target = secrets[0]
    sid = target["id"]
    print(f"\n[5] EXPORT_SECRET id={sid} type=0x{target['type']:02X}...")
    data, sw1, sw2 = transmit(export_secret_init(sid))
    if (sw1, sw2) != (0x90, 0x00):
        print(f"  EXPORT init failed: SW={sw1:02X}{sw2:02X}")
        return
    exported = b""
    while True:
        data, sw1, sw2 = transmit(export_secret_update(sid))
        if (sw1, sw2) != (0x90, 0x00):
            break
        if len(data) >= 2:
            ln = (data[0] << 8) | data[1]
            chunk = data[2:2 + ln]
            exported += chunk
            if ln == 0:
                break
        else:
            break
    print(f"  Exported {len(exported)} bytes")
    if target["type"] == 0x30 and exported:
        try:
            from mnemonic import Mnemonic
            m = Mnemonic("english")
            words = m.to_mnemonic(exported)
            print(f"  BIP39: {words}")
        except Exception as e:
            print(f"  (BIP39 decode failed: {e})")
            print(f"  Raw hex: {exported.hex()}")
    elif exported:
        print(f"  Hex: {exported.hex()[:80]}..." if len(exported.hex()) > 80 else f"  Hex: {exported.hex()}")

    print("\nDone.")


if __name__ == "__main__":
    main()
