#!/usr/bin/env python3
"""
Read F746 diagnostic buffer via CCID Escape command (0x6B).

Sends PC_to_RDR_Escape and decodes the TLV diagnostic data
returned in the RDR_to_PC_Escape (0x83) response.

Usage:
    python3 scripts/read_diag.py [bus device]
    python3 scripts/read_diag.py        # auto-detect first CCID device

Requires: pip install pyusb
"""

import struct
import sys
import usb.core


CCID_HEADER_SIZE = 10
PC_TO_RDR_ESCAPE = 0x6B
RDR_TO_PC_ESCAPE = 0x83



def find_ccid_device(bus=None, addr=None):
    # CCID is an interface class (0x0B), not device class. Search by known VIDs.
    VIDS = [0x046a, 0x08e6]
    for vid in VIDS:
        dev = usb.core.find(idVendor=vid)
        if dev:
            if bus is not None and (dev.bus != bus or dev.address != addr):
                continue
            return dev
    return None


def send_escape(dev):
    # Escape 0xD0: one payload byte (the escape code) after the 10-byte
    # CCID header — dwLength must be 1, not 0 (empty escape payloads are
    # rejected before the 0xD0 branch, so the old request could never
    # reach the diagnostic handler).
    req = bytearray(CCID_HEADER_SIZE + 1)
    req[0] = PC_TO_RDR_ESCAPE
    struct.pack_into("<I", req, 1, 1)  # dwLength = 1
    req[5] = 0  # slot
    req[6] = 1  # seq
    req[7] = 0
    req[8] = 0
    req[9] = 0
    req[CCID_HEADER_SIZE] = 0xD0

    cfg = dev.get_active_configuration()
    ep_out = None
    ep_in = None
    for intf in cfg:
        if intf.bInterfaceClass == 0x0B:
            dev.detach_kernel_driver(intf.bInterfaceNumber) if dev.is_kernel_driver_active(intf.bInterfaceNumber) else None
            usb.util.claim_interface(dev, intf.bInterfaceNumber)
            for ep in intf:
                is_in = ep.bEndpointAddress & 0x80 != 0
                is_bulk = (ep.bmAttributes & 0x03) == 2
                if is_in and is_bulk:
                    ep_in = ep
                elif not is_in and is_bulk:
                    ep_out = ep
            break

    if not ep_out or not ep_in:
        print("ERROR: Could not find CCID bulk endpoints")
        sys.exit(1)

    ep_out.write(req)
    resp = ep_in.read(512, timeout=5000)

    if resp[0] != RDR_TO_PC_ESCAPE:
        print(f"ERROR: Expected RDR_TO_PC_ESCAPE (0x83), got 0x{resp[0]:02X}")
        sys.exit(1)

    data_len = struct.unpack_from("<I", resp, 1)[0]
    status = resp[7]
    error = resp[8]
    data = bytes(resp[CCID_HEADER_SIZE:CCID_HEADER_SIZE + data_len])

    if error != 0:
        print(f"CCID error: status=0x{status:02X} error=0x{error:02X}")

    return data


def format_hex(data):
    return " ".join(f"{b:02X}" for b in data)


def interpret_tag(tag, payload):
    name = TAG_NAMES.get(tag, f"UNKNOWN(0x{tag:02X})")

    if tag == DTAG_IO_READBACK:
        high_ok = payload[0] if len(payload) > 0 else "?"
        low_ok = payload[1] if len(payload) > 1 else "?"
        verdict = "OK" if high_ok == 1 and low_ok == 1 else "FAIL"
        return f"{name}: high={high_ok} low={low_ok} [{verdict}]"

    if tag == DTAG_ATR:
        atr_hex = format_hex(payload)
        return f"{name} ({len(payload)} bytes): {atr_hex}"

    if tag == DTAG_TX_SINGLE:
        before = payload[0] if len(payload) > 0 else "?"
        after = payload[1] if len(payload) > 1 else "?"
        result = payload[2] if len(payload) > 2 else "?"
        result_str = {0: "CARD_RESPONDED", 1: "TIMEOUT", 2: "ERROR"}.get(result, f"UNKNOWN({result})")
        return f"{name}: before_high={before} after_high={after} result={result_str}"

    if tag == DTAG_TX_BYTE_ERR:
        byte_val = payload[0] if len(payload) > 0 else "?"
        low_ok = payload[1] if len(payload) > 1 else "?"
        high_ok = payload[2] if len(payload) > 2 else "?"
        return f"{name}: byte=0x{byte_val:02X} low_ok={low_ok} high_ok={high_ok}"

    if tag == DTAG_DWT_STAMP:
        if len(payload) >= 4:
            stamp = struct.unpack_from("<I", payload)[0]
            us = stamp / 216.0
            return f"{name}: cyccnt={stamp} (~{us:.0f}us)"
        return f"{name}: {format_hex(payload)}"

    if tag == DTAG_END:
        return f"{name}"

    return f"{name}: {format_hex(payload)}"


def main():
    parser = argparse.ArgumentParser(
        description="Read the 0xD0 diagnostic counters from a CCID reader")
    parser.add_argument("--vid", type=lambda x: int(x, 16), default=0x046A,
                        help="USB vendor ID (default 046A Cherry)")
    parser.add_argument("--pid", type=lambda x: int(x, 16), default=None,
                        help="USB product ID (optional)")
    parser.add_argument("--bus", type=int, default=None)
    parser.add_argument("--address", type=int, default=None)
    args = parser.parse_args()

    dev = find_ccid_device(args.bus, args.address)
    if dev is None:
        print("ERROR: device not found")
        sys.exit(1)

    data = send_escape(dev)

    # 28-byte little-endian Diagnostics struct (crates/ccid-core
    # diagnostics.rs SERIALIZED_SIZE = 28)
    if len(data) < 28:
        print(f"ERROR: expected 28-byte diagnostics struct, got {len(data)}")
        print(f"raw: {format_hex(data)}")
        sys.exit(1)

    tx, rx, nak, err, reinit, present, uptime = struct.unpack_from("<7I", data)
    print("Diagnostics (Escape 0xD0):")
    print(f"  apdu_tx_count : {tx}")
    print(f"  apdu_rx_count : {rx}")
    print(f"  nak_count     : {nak} (host serial framing NAKs)")
    print(f"  error_count   : {err}")
    print(f"  reinit_count  : {reinit}")
    print(f"  card_present  : {bool(present)}")
    print(f"  uptime_ticks  : {uptime}")


if __name__ == "__main__":
    main()
