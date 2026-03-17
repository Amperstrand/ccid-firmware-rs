#!/usr/bin/env python3
"""Capture raw CCID USB bulk transfers from Gemalto reader while running a session."""

import json
import time
import struct
import threading
import sys
import os
import signal

import usb.core
import usb.util
from smartcard.System import readers
from smartcard.util import toBytes

GEMALTO_VID = 0x08E6
GEMALTO_PID = 0x3437
CCID_BULK_EP_SIZE = 64

CAPTURE_DIR = os.path.dirname(os.path.abspath(__file__))
CAPTURE_FILE = os.path.join(CAPTURE_DIR, "seedkeeper_raw.json")

captured = []
capture_lock = threading.Lock()
capture_active = True
dev = None
ep_in = None
ep_out = None

def find_reader():
    global dev, ep_in, ep_out
    devs = list(usb.core.find(find_all=True, idVendor=GEMALTO_VID, idProduct=GEMALTO_PID))
    if not devs:
        print("Gemalto reader not found via USB!", file=sys.stderr)
        return False
    d = devs[0]
    dev = d
    cfg = d.get_active_configuration()
    intf = cfg[(0, 0)]
    ep_in = usb.util.find_descriptor(intf, bEndpointAddress=(0x02 | usb.util.ENDPOINT_IN))
    ep_out = usb.util.find_descriptor(intf, bEndpointAddress=0x01)
    if not ep_in or not ep_out:
        print("Could not find CCID bulk endpoints!", file=sys.stderr)
        return False
    print(f"Found reader: {d} | EP_IN=0x{ep_in.bEndpointAddress:02X} EP_OUT=0x{ep_out.bEndpointAddress:02X}")
    return True

def capture_thread():
    """Capture USB bulk transfers in background."""
    global capture_active
    while capture_active:
        try:
            data = dev.read(ep_in.bEndpointAddress, CCID_BULK_EP_SIZE, timeout=100)
            with capture_lock:
                captured.append({
                    "direction": "reader_to_host",
                    "timestamp_us": int(time.monotonic() * 1_000_000),
                    "data": data.hex(),
                })
        except usb.core.USBError as e:
            if e.errno == 110:  # timeout
                continue
            if capture_active:
                print(f"USB read error: {e}", file=sys.stderr)
            break

def hex_to_bytes(h):
    if isinstance(h, str):
        return bytes.fromhex(h)
    return bytes(h)

def send_pyscard_session():
    """Run a CCID session via pyscard."""
    global capture_active
    time.sleep(1)  # Let capture thread start
    
    try:
        r = readers()
        if not r:
            print("No readers via pyscard!", file=sys.stderr)
            return
        
        reader = r[0]
        print(f"pyscard reader: {reader}")
        
        conn = reader.createConnection()
        
        # Power on (connect)
        print("1. Power On...")
        conn.connect()
        atr = conn.getATR()
        print(f"   ATR: {hex_to_bytes(atr).hex()}")
        time.sleep(0.3)
        
        # Get slot status
        print("2. Get Slot Status...")
        status = conn.getStatus()
        print(f"   Status: {status}")
        time.sleep(0.3)
        
        # SELECT SeedKeeper AID
        print("3. SELECT SeedKeeper AID...")
        try:
            data, sw1, sw2 = conn.transmit(toBytes("A0 00 00 00 62 01 01 00"))
            print(f"   Response: {hex_to_bytes(data).hex()} SW={sw1:02X}{sw2:02X}")
        except Exception as e:
            print(f"   Error: {e}")
        time.sleep(0.3)
        
        # GET DATA
        print("4. GET DATA...")
        try:
            data, sw1, sw2 = conn.transmit(toBytes("00 CA 01 00 00"))
            print(f"   Response: {hex_to_bytes(data).hex()} SW={sw1:02X}{sw2:02X}")
        except Exception as e:
            print(f"   Error: {e}")
        time.sleep(0.3)
        
        # VERIFY PIN (1234)
        print("5. VERIFY PIN (1234)...")
        try:
            data, sw1, sw2 = conn.transmit(toBytes("00 20 00 81 04 31 32 33 34 FF"))
            print(f"   Response: {hex_to_bytes(data).hex()} SW={sw1:02X}{sw2:02X}")
        except Exception as e:
            print(f"   Error: {e}")
        time.sleep(0.3)
        
        # Disconnect
        print("6. Power Off...")
        conn.disconnect()
        time.sleep(0.5)
        
    except Exception as e:
        print(f"Session error: {e}", file=sys.stderr)
    finally:
        capture_active = False

def classify_message(data_hex):
    """Classify a CCID message by type."""
    msg_type = int(data_hex[:2], 16)
    type_map = {
        0x62: "PC_TO_RDR_ICC_POWER_ON",
        0x63: "PC_TO_RDR_ICC_POWER_OFF",
        0x65: "PC_TO_RDR_GET_SLOT_STATUS",
        0x6F: "PC_TO_RDR_XFR_BLOCK",
        0x6C: "PC_TO_RDR_GET_PARAMETERS",
        0x61: "PC_TO_RDR_SET_PARAMETERS",
        0x6D: "PC_TO_RDR_RESET_PARAMETERS",
        0x69: "PC_TO_RDR_SECURE",
        0x6B: "PC_TO_RDR_ESCAPE",
        0x6E: "PC_TO_RDR_ICC_CLOCK",
        0x6A: "PC_TO_RDR_T0_APDU",
        0x71: "PC_TO_RDR_MECHANICAL",
        0x72: "PC_TO_RDR_ABORT",
        0x73: "PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ",
        0x80: "RDR_TO_PC_DATABLOCK",
        0x81: "RDR_TO_PC_SLOTSTATUS",
        0x82: "RDR_TO_PC_PARAMETERS",
        0x83: "RDR_TO_PC_ESCAPE",
        0x84: "RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ",
        0x50: "RDR_TO_PC_NOTIFY_SLOT_CHANGE",
    }
    return type_map.get(msg_type, f"UNKNOWN_0x{msg_type:02X}")

def parse_ccid_fields(data_hex):
    """Parse CCID header fields from hex string."""
    if len(data_hex) < 20:
        return {}
    data = bytes.fromhex(data_hex)
    dw_length = struct.unpack_from('<I', data, 1)[0]
    return {
        "bMessageType": f"0x{data[0]:02X}",
        "bMessageTypeName": classify_message(data_hex),
        "dwLength": dw_length,
        "bSlot": data[5],
        "bSeq": data[6],
        "bStatus": f"0x{data[7]:02X}" if len(data) > 7 else None,
        "bError": f"0x{data[8]:02X}" if len(data) > 8 else None,
        "bClockStatus": f"0x{data[9]:02X}" if len(data) > 9 else None,
        "abData": data[10:10 + dw_length].hex() if dw_length > 0 and len(data) >= 10 + dw_length else "",
    }

def main():
    global capture_active
    
    print("=" * 60)
    print("CCID Raw USB Capture - SeedKeeper on Gemalto PC Twin")
    print("=" * 60)
    
    if not find_reader():
        return 1
    
    # Claim the USB interface (detach from kernel driver)
    try:
        usb.util.claim_interface(dev, 0)
        print("USB interface claimed (detached from kernel)")
    except usb.core.USBError as e:
        print(f"Warning: Could not claim interface: {e}")
        print("pcscd may hold the device. Continuing anyway...")
    
    # Start capture thread
    print("Starting USB capture thread...")
    cap_thread = threading.Thread(target=capture_thread, daemon=True)
    cap_thread.start()
    
    # Run pyscard session (this sends CCID commands via pcscd)
    print("Starting pyscard session...")
    send_pyscard_session()
    
    # Wait for capture thread to finish
    cap_thread.join(timeout=3)
    capture_active = False
    
    # Release interface
    try:
        usb.util.release_interface(dev, 0)
    except:
        pass
    
    # Process and save captures
    with capture_lock:
        messages = list(captured)
    
    print(f"\nCaptured {len(messages)} USB transfers")
    
    # Classify and structure the capture
    structured = []
    for i, msg in enumerate(messages):
        parsed = parse_ccid_fields(msg["data"])
        structured.append({
            "id": i,
            "direction": msg["direction"],
            "ccid_bytes": msg["data"],
            "msg_type": parsed.get("bMessageType", ""),
            "msg_type_name": parsed.get("bMessageTypeName", ""),
            "dw_length": parsed.get("dwLength", 0),
            "b_slot": parsed.get("bSlot", 0),
            "b_seq": parsed.get("bSeq", 0),
            "b_status": parsed.get("bStatus"),
            "b_error": parsed.get("b_error"),
            "ab_data": parsed.get("abData", ""),
        })
    
    # Identify request-response pairs
    print("\nCCID Message Sequence:")
    for msg in structured:
        dir_arrow = ">>>" if msg["direction"] == "host_to_reader" else "<<<"
        print(f"  {dir_arrow} [{msg['msg_type']}] {msg['msg_type_name']}")
        if msg["ab_data"]:
            data_preview = msg["ab_data"][:64]
            print(f"       Data: {data_preview}{'...' if len(msg['ab_data']) > 64 else ''}")
    
    # Save capture
    capture = {
        "metadata": {
            "reader": "Gemalto PC Twin Reader",
            "reader_vid": "08E6",
            "reader_pid": "3437",
            "card": "SeedKeeper",
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "total_messages": len(structured),
            "capture_method": "raw_usb_bulk",
        },
        "messages": structured,
    }
    
    with open(CAPTURE_FILE, 'w') as f:
        json.dump(capture, f, indent=2)
    
    print(f"\nCapture saved to {CAPTURE_FILE}")
    
    # Summary
    host_msgs = [m for m in structured if m["direction"] == "host_to_reader"]
    reader_msgs = [m for m in structured if m["direction"] == "reader_to_host"]
    print(f"  Host -> Reader: {len(host_msgs)}")
    print(f"  Reader -> Host: {len(reader_msgs)}")
    
    return 0

if __name__ == "__main__":
    sys.exit(main())
