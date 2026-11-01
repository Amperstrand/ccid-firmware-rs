#!/usr/bin/env python3
"""hce_relay.py — host-side card brain for the bench HCE relay (#529).

The phone (com.amperstrand.benchrelay, host-tools/hce-relay-app) is an
RF modem: it forwards every C-APDU the reader sends to this process and
returns whatever we answer. Frames: 2-byte BIG-ENDIAN length + payload
(the org.amperstrand.VpcdBridge framing). Control frames from the phone:
0x01 = ISO-DEP link deactivated (field off / re-poll), 0x02 = hello.

Transport: adb forward tcp:4871 localabstract:bench-hce-relay

Modes (the bench instruments from #529):
  --respond HEX        answer every C-APDU with HEX (default 9000)
  --script FILE        lines "CAPDU_HEX=RAPDU_HEX"; unmatched -> 6A82
  --delay-ms N         sleep N ms before answering (the #472 slow-card /
                       time-extension instrument; also WTX-timing tests)
  --once N             exit after N exchanges

Place/remove verbs (see #529): svc nfc enable/disable on the phone.
"""
import argparse
import socket
import subprocess
import sys
import time

HOST_PORT = 4871
SOCKET_NAME = "bench-hce-relay"


def adb(*args: str) -> str:
    return subprocess.run(["adb", *args], capture_output=True,
                          text=True, timeout=30).stdout.strip()


def connect(serial: str | None) -> socket.socket:
    base = ["-s", serial] if serial else []
    adb(*base, "forward", f"tcp:{HOST_PORT}", f"localabstract:{SOCKET_NAME}")
    for _ in range(20):
        try:
            s = socket.create_connection(("127.0.0.1", HOST_PORT), timeout=2)
            s.settimeout(None)  # block indefinitely waiting for reader traffic
            return s
        except OSError:
            time.sleep(0.5)
    raise SystemExit("cannot reach the relay socket — is the app "
                     "installed and NFC on?")


def recv_exact(s: socket.socket, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise EOFError("phone closed the relay socket")
        buf += chunk
    return buf


def recv_frame(s: socket.socket) -> bytes:
    return recv_exact(s, int.from_bytes(recv_exact(s, 2), "big"))


def send_frame(s: socket.socket, payload: bytes) -> None:
    s.sendall(len(payload).to_bytes(2, "big") + payload)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--serial", default=None, help="adb device serial")
    ap.add_argument("--respond", default="9000",
                    help="fixed R-APDU hex for --respond mode (default 9000)")
    ap.add_argument("--script", default=None,
                    help="file of CAPDU_HEX=RAPDU_HEX lines")
    ap.add_argument("--delay-ms", type=int, default=0)
    ap.add_argument("--once", type=int, default=0,
                    help="exit after N exchanges")
    args = ap.parse_args()

    table: dict[bytes, bytes] = {}
    if args.script:
        for line in open(args.script):
            line = line.split("#")[0].strip()
            if "=" in line:
                capdu, rapdu = line.split("=", 1)
                table[bytes.fromhex(capdu.strip())] = \
                    bytes.fromhex(rapdu.strip())
        print(f"script: {len(table)} entries")
    else:
        fixed = bytes.fromhex(args.respond)
        print(f"respond mode: every C-APDU -> {fixed.hex()}")

    s = connect(args.serial)
    print(f"connected (tcp:{HOST_PORT} -> localabstract:{SOCKET_NAME})")
    done = 0
    try:
        while args.once <= 0 or done < args.once:
            frame = recv_frame(s)
            if len(frame) == 1 and frame[0] == 0x02:
                print("[hello] phone relay is up")
                continue
            if len(frame) == 1 and frame[0] == 0x01:
                print("[deactivated] ISO-DEP link down (re-poll/field-off)")
                continue
            print(f"C-APDU <- {frame.hex()}")
            if args.delay_ms:
                time.sleep(args.delay_ms / 1000.0)
            if table:
                resp = table.get(frame, bytes.fromhex("6a82"))
            else:
                resp = fixed
            send_frame(s, resp)
            print(f"R-APDU -> {resp.hex()}")
            done += 1
    except KeyboardInterrupt:
        pass
    except EOFError as e:
        print(f"relay dropped: {e}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
