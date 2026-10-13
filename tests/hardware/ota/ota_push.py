#!/usr/bin/env python3
"""OTA push client library + CLI for the wifi-vpcd boards (TCP :3232).

ATTRIBUTION: copied from the bench-proven host client
/tmp/opencode/idf555/ota_push.py (2026-10-13) as the library module for
this suite, because the repo has no host-tools/ota_push.py at this commit.
Adaptations from the original (each marked "ADAPTED" below):
  - added push_ex(): a structured push used by the test suite that reports
    rejected/accepted instead of an exit code and does no liveness polling
    (recovery waiting belongs to the suite, which uses generous windows);
  - the original post-flash liveness probe checked the retired :29443
    CCID port; the CLI now checks the OTA server port :3232 itself, which
    both bench boards serve (firmware/esp32-ccid/src/ota.rs);
  - push_ex() accepts image bytes OR a filesystem path.

Wire protocol (firmware/esp32-ccid/src/ota.rs):
  connect -> "OTA1" + u32-LE image size -> raw image bytes
  board writes the inactive OTA slot, switches boot partition, reboots.
  On error the board answers a single 'E' byte before closing:
  esp_ota_end() runs a full esp_image_verify (magic, segments, checksum,
  appended SHA-256) BEFORE esp_ota_set_boot_partition(), so a corrupt or
  truncated image is rejected device-side and the running image survives.

Exit codes (CLI): 0 = accepted + board alive again, 1 = rejected or
socket error, 2 = accepted but the board did not look alive within the
wait window (informational - the suite runs its own recovery logic).

Usage: python3 ota_push.py <board-ip> <image.bin> [--port N]
"""

import socket
import struct
import sys
import time

MAGIC = b"OTA1"
DEFAULT_PORT = 3232
#: firmware/esp32-ccid/src/ota.rs: hard server-side limit.
MAX_IMAGE_SIZE = 4 * 1024 * 1024


def push_ex(ip, image, port=DEFAULT_PORT, connect_timeout=10, ack_timeout=20):
    """Push an image; report the raw protocol outcome.

    `image` is bytes or a path (str/Path). Returns a dict:
      connected  bool   TCP connect to the OTA server succeeded
      sent       int    image bytes streamed
      total      int    announced image size
      rejected   bool   board replied 'E' (esp_ota_end validation failed -
                       the running image was never touched)
      accepted   bool   stream completed and the board went silent
                       (EOF/timeout = reboot path; ota.rs sends no ack
                       byte on success)
      error      str    socket-phase failure description, else None
      elapsed_s  float  wall time of the send+ack phases
    """
    if isinstance(image, (bytes, bytearray)):
        data = bytes(image)
    else:
        with open(image, "rb") as f:
            data = f.read()
    if not data:
        return {"connected": False, "sent": 0, "total": 0, "rejected": False,
                "accepted": False, "error": "empty image", "elapsed_s": 0.0}
    if len(data) > MAX_IMAGE_SIZE:
        return {"connected": False, "sent": 0, "total": len(data), "rejected": False,
                "accepted": False,
                "error": f"image {len(data)} B exceeds server limit {MAX_IMAGE_SIZE} B",
                "elapsed_s": 0.0}

    out = {"connected": False, "sent": 0, "total": len(data),
           "rejected": False, "accepted": False, "error": None, "elapsed_s": 0.0}
    t0 = time.time()
    try:
        s = socket.create_connection((ip, port), timeout=connect_timeout)
    except OSError as e:
        out["error"] = f"connect failed: {e}"
        out["elapsed_s"] = time.time() - t0
        return out
    out["connected"] = True
    try:
        s.sendall(MAGIC + struct.pack("<I", len(data)))
        sent = 0
        while sent < len(data):
            chunk = data[sent:sent + 4096]
            s.sendall(chunk)
            sent += len(chunk)
        out["sent"] = sent
        s.settimeout(ack_timeout)
        try:
            resp = s.recv(1)
            if resp == b"E":
                out["rejected"] = True
            elif resp:
                out["error"] = f"unexpected response byte {resp.hex()}"
            else:
                # EOF right after the stream: ota.rs rebooted without an
                # ack - the accept path.
                out["accepted"] = True
        except socket.timeout:
            # Board neither acked nor rejected within the window. ota.rs
            # reboots ~0.5 s after esp_ota_set_boot_partition(); a timeout
            # this long usually means the reboot already severed us.
            out["accepted"] = True
    except OSError as e:
        out["error"] = f"send failed after {out['sent']} B: {e}"
    finally:
        try:
            s.close()
        except OSError:
            pass
    out["elapsed_s"] = time.time() - t0
    return out


def push(ip, path, port=DEFAULT_PORT, alive_wait_s=30):
    """CLI flow: push, then poll the board's OTA port for liveness."""
    print(f"image: {path}")
    print(f"connecting to {ip}:{port} ...")
    out = push_ex(ip, path, port=port)
    if out["error"] and not out["connected"]:
        print(f"ERROR: {out['error']}")
        return 1
    if out["sent"]:
        print(f"sent {out['sent']} bytes in {out['elapsed_s']:.1f}s")
    if out["rejected"]:
        print("ERROR: board rejected the update ('E' - image failed "
              "esp_ota_end validation; running image untouched)")
        return 1
    if out["error"]:
        print(f"ERROR: {out['error']}")
        return 1
    print("image accepted; board is rebooting ...")
    # ADAPTED: liveness probe is the OTA server port itself (:3232), not
    # the retired :29443 CCID port of the original client.
    deadline = time.time() + alive_wait_s
    while time.time() < deadline:
        try:
            with socket.create_connection((ip, port), timeout=2):
                print("SUCCESS: OTA server is accepting connections again")
                return 0
        except OSError:
            time.sleep(2)
    print(f"WARNING: OTA server on :{port} not back within {alive_wait_s}s")
    return 2


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print(f"usage: {sys.argv[0]} <ip> <image.bin> [--port N]")
        sys.exit(1)
    port = int(sys.argv[4]) if len(sys.argv) > 4 and sys.argv[3] == "--port" else DEFAULT_PORT
    sys.exit(push(sys.argv[1], sys.argv[2], port=port))
