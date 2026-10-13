#!/usr/bin/env python3
"""OTA push client for the nucula vpcd board (TCP :3232).

Copied from the bench-proven host-tools client (2026-10-13) and adapted:
the post-flash liveness check looks for the vpcd listener session instead
of the retired :29443 CCID port — wifi-vpcd boards connect OUT to the
host's libifdvpcd listener (docs/VPCD_WIFI_RUNBOOK.md "Architecture").

Wire protocol (firmware/esp32-ccid/src/ota.rs):
  connect -> "OTA1" + u32-LE image size -> raw image bytes
  board writes the inactive OTA slot, switches boot partition, reboots.
  On error the board answers 'E' before closing.

Exit codes: 0 = image accepted by the board, 1 = rejected/socket error,
2 = pushed but the board did not look alive again within the wait window
(informational for the test suite, which runs its own recovery logic).

Usage: python3 ota_push.py <board-ip> <image.bin> [--vpcd-host-port 35963]
"""

import socket
import struct
import sys
import time


def vpcd_estab(port):
    """Any ESTAB session on the vpcd listener (board connects out)."""
    try:
        with open("/proc/net/tcp", "r") as f:  # no ss dependency
            for line in f.readlines()[1:]:
                parts = line.split()
                if parts[3] != "01":  # 01 = ESTABLISHED
                    continue
                local = parts[1].split(":")[1]
                if int(local, 16) == port:
                    return True
    except OSError:
        pass
    return False


def push(ip, path, vpcd_host_port=35963, port=3232):
    image = open(path, "rb").read()
    print(f"image: {path} ({len(image)} bytes)")
    print(f"connecting to {ip}:{port} ...")
    s = socket.create_connection((ip, port), timeout=10)
    s.sendall(b"OTA1" + struct.pack("<I", len(image)))
    print("header sent, streaming image ...")
    sent = 0
    t0 = time.time()
    while sent < len(image):
        chunk = image[sent:sent + 4096]
        s.sendall(chunk)
        sent += len(chunk)
        if sent % (64 * 1024) < 4096:
            print(f"  {sent}/{len(image)} ({sent * 100 // len(image)}%)")
    elapsed = time.time() - t0
    print(f"sent {sent} bytes in {elapsed:.1f}s ({sent / elapsed / 1024:.0f} KiB/s)")
    s.settimeout(15)
    try:
        resp = s.recv(1)
        if resp == b"E":
            print("ERROR: board rejected the update")
            return 1
        elif resp:
            print(f"unexpected response: {resp.hex()}")
            return 1
    except socket.timeout:
        pass  # board rebooted mid-send — normal
    print("image accepted; board is rebooting ...")
    time.sleep(8)
    for attempt in range(11):
        if vpcd_estab(vpcd_host_port):
            print(f"SUCCESS: vpcd listener session back (attempt {attempt + 1})")
            return 0
        time.sleep(2)
    print("WARNING: no ESTAB session on the vpcd listener within ~30s "
          "(may be a stale half-open socket on the host — see README)")
    return 2


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print(f"usage: {sys.argv[0]} <ip> <image.bin> [--vpcd-host-port N]")
        sys.exit(1)
    port = int(sys.argv[4]) if len(sys.argv) > 4 and sys.argv[3] == "--vpcd-host-port" else 35963
    sys.exit(push(sys.argv[1], sys.argv[2], vpcd_host_port=port))
