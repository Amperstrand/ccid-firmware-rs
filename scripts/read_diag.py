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

DTAG_IO_READBACK = 0x01
DTAG_ATR = 0x02
DTAG_TX_SINGLE = 0x03
DTAG_TX_BYTE_ERR = 0x04
DTAG_DWT_STAMP = 0x05
DTAG_END = 0xFF

TAG_NAMES = {
    DTAG_IO_READBACK: "IO_READBACK",
    DTAG_ATR: "ATR",
    DTAG_TX_SINGLE: "TX_SINGLE",
    DTAG_TX_BYTE_ERR: "TX_BYTE_ERR",
    DTAG_DWT_STAMP: "DWT_STAMP",
    DTAG_END: "END",
}


def find_ccid_device(bus=None, addr=None):
    devs = usb.core.find(find_all=True, bDeviceClass=0x0B)
    for d in devs:
        if bus is not None and (d.bus != bus or d.address != addr):
            continue
        return d
    return None


def send_escape(dev):
    req = bytearray(CCID_HEADER_SIZE)
    req[0] = PC_TO_RDR_ESCAPE
    # dwLength = 0
    req[5] = 0  # slot
    req[6] = 1  # seq
    req[7] = 0
    req[8] = 0
    req[9] = 0

    cfg = dev.get_active_configuration()
    ep_out = None
    ep_in = None
    for intf in cfg:
        if intf.bInterfaceClass == 0x0B:
            dev.detach_kernel_driver(intf.bInterfaceNumber) if dev.is_kernel_driver_active(intf.bInterfaceNumber) else None
            usb.util.claim_interface(dev, intf.bInterfaceNumber)
            for ep in intf:
                if ep.bEndpointAddress & 0x80:
                    ep_in = ep
                else:
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


def decode_tlv(data):
    offset = 0
    entries = []
    while offset + 2 <= len(data):
        tag = data[offset]
        length = data[offset + 1]
        if offset + 2 + length > len(data):
            print(f"  TRUNCATED: tag=0x{tag:02X} len={length} but only {len(data) - offset - 2} bytes remain")
            break
        payload = data[offset + 2:offset + 2 + length]
        entries.append((tag, payload))
        offset += 2 + length

    return entries


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
    bus = None
    addr = None
    if len(sys.argv) >= 3:
        bus = int(sys.argv[1])
        addr = int(sys.argv[2])

    dev = find_ccid_device(bus, addr)
    if not dev:
        print("No CCID device found")
        sys.exit(1)

    print(f"Device: {dev.bus}/{dev.address} {dev.idVendor:04X}:{dev.idProduct:04X}")

    try:
        data = send_escape(dev)
    finally:
        usb.util.dispose_resources(dev)

    if not data:
        print("Empty diagnostic buffer (no power-on yet?)")
        return

    print(f"\nDiagnostic buffer: {len(data)} bytes")
    print(f"Raw: {format_hex(data)}\n")

    entries = decode_tlv(data)
    for tag, payload in entries:
        print(f"  {interpret_tag(tag, payload)}")

    if not entries:
        print("  (no TLV entries found)")


if __name__ == "__main__":
    main()
