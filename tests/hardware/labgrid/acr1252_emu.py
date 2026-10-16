"""ACR1252 card-emulation fixture (NFC Forum Type-2 / MIFARE Ultralight).

STATUS (2026-10-16, overnight bench session — ccid issue #<auto>): the
control plane works to the CCID layer; the emulated tag is NOT yet
observed by bench readers. Details:

- pcscd CANNOT reach the escape interface: the ACR1252U does not
  advertise FEATURE_CCID_ESC (features TLV: 0x12, 0x13 only), so
  SCardControl returns "Feature not supported" for every escape code.
- Transport that DOES reach the reader: raw USB CCID PC_to_RDR_Escape
  (type 0x6B) on interface 1 (PICC) via pyusb, with pcscd.socket +
  pcscd.service stopped for the window (sudo -n works on the bench
  runner; the flock/place acquisition in bench.py covers exclusivity —
  same discipline as the pcscd battery toggle).
- With a CORRECT CCID header ([type][dwLength LE][slot][seq][RFU2]),
  the reader accepts the ENTER command (E0 00 00 40 03 01 00 00) but
  returns no payload within 800 ms — unknown whether emulation engaged
  (firmware may drop the reply while switching the RF front-end) or
  the command was ignored. The ACS-documented ack is
  E1 00 00 00 03 01 01 01.

NEXT SESSION (verify in this order):
1. RF coupling: confirm the ACR1252 antenna is physically within range
   of the nucula PN7160 and the m5stick RC522 (user reports shared
   field; unverified remotely).
2. Enter emulation, restart pcscd, watch `SCardGetStatusChange` on the
   Nucula VPCD slots for card arrival (first run: no card observed).
3. If still dark: try the ACS Android-library byte order for the enter
   command, and the §5.6.2 Read Card Emulation Data escape as a
   control-plane probe (does not need RF).
4. ISO-DEP (Type-4 / NTAG424) flows are NOT emulatable on this
   hardware — those stay on the real-card plan regardless.

Commands (ACR1252U API §5.6):
  enter:  E0 00 00 40 03 01 00 00
  write:  E0 00 00 60 <Lc> 01 01 <addr> <len> <data...>
  exit:   E0 00 00 40 03 00 00 00

Write byte-address 0 maps to block 3 (capability container): a full
NDEF image = CC (E1 10 06 00) + NDEF TLV (03 <len> <msg>) + terminator
(FE) + zero padding.
"""

from __future__ import annotations

import os
import shlex
import time

HELPER_DIR = f"/tmp/hil-ccid-bench-{os.getuid()}"
HELPER_PATH = f"{HELPER_DIR}/acr_emu.py"


class ACR1252EmulationError(RuntimeError):
    pass


HELPER = '''#!/usr/bin/env python3
"""acr_emu.py — standalone ACR1252 raw-CCID escape helper (bench host).

usage: acr_emu.py enter|exit|write <hexdata>
NEEDS pcscd stopped (sudo -n systemctl stop pcscd.socket pcscd.service);
caller is responsible for stop/start around this process.
"""
import sys

import usb.core
import usb.util

VID_PID = (0x072F, 0x223B)


def die(msg):
    print(msg, file=sys.stderr)
    sys.exit(1)


def open_picc():
    dev = usb.core.find(idVendor=VID_PID[0], idProduct=VID_PID[1])
    if dev is None:
        die("ACR1252 not present on USB")
    for ifnum in (0, 1):
        try:
            if dev.is_kernel_driver_active(ifnum):
                dev.detach_kernel_driver(ifnum)
        except Exception:
            pass
    dev.set_configuration()
    intf = list(dev.get_active_configuration().interfaces())[1]  # PICC
    ep_out = [e for e in intf
              if usb.util.endpoint_direction(e.bEndpointAddress) == usb.util.ENDPOINT_OUT][0]
    ep_in = [e for e in intf
             if usb.util.endpoint_direction(e.bEndpointAddress) == usb.util.ENDPOINT_IN][0]
    return dev, ep_out, ep_in


_seq = 0


def escape(ep_out, ep_in, data, reply_timeout_ms=800):
    global _seq
    msg = (bytes([0x6B]) + len(data).to_bytes(4, "little")
           + bytes([0x00, _seq, 0x00, 0x00]) + data)
    _seq = (_seq + 1) & 0xFF
    ep_out.write(msg, timeout=5000)
    try:
        hdr = ep_in.read(512, timeout=reply_timeout_ms)
    except usb.core.USBTimeoutError:
        return None  # firmware observed to swallow the enter-emu reply
    blen = int.from_bytes(bytes(hdr[1:5]), "little")
    return bytes(hdr[10:10 + blen])


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    dev, ep_out, ep_in = open_picc()
    if cmd == "enter":
        out = escape(ep_out, ep_in, bytes.fromhex("E000004003010000"))
        print("OK" if out is not None else "SENT-NO-REPLY")
    elif cmd == "exit":
        escape(ep_out, ep_in, bytes.fromhex("E000004003000000"))
        print("OK")
    elif cmd == "write":
        data = bytes.fromhex(sys.argv[2])
        body = bytes([0x01, 0x01, 0x00, len(data)]) + data
        apdu = bytes.fromhex("E0000060") + bytes([len(body)]) + body
        out = escape(ep_out, ep_in, apdu, reply_timeout_ms=3000)
        if out is None or not out.endswith(bytes.fromhex("9000")):
            die(f"write not confirmed: {out.hex() if out else 'no-reply'}")
        print("OK")
    else:
        die("usage: acr_emu.py enter|exit|write <hex>")


if __name__ == "__main__":
    main()
'''


def ndef_uri_block_image(uri: str) -> str:
    prefix, rest = 0x04, uri
    for p, tag in ((0x04, "https://"), (0x03, "http://")):
        if uri.startswith(tag):
            prefix, rest = p, uri[len(tag):]
            break
    payload = bytes([prefix]) + rest.encode()
    record = bytes([0xD1, 0x01, len(payload), ord("U")]) + payload
    data = bytes.fromhex("E1100600") + b"\x03" + bytes([len(record)]) + record + b"\xFE"
    data += b"\x00" * ((-len(data)) % 4)
    return data.hex()


def _pcscd(bench, action: str) -> None:
    r = bench.run(f"sudo -n systemctl {action} pcscd.socket pcscd.service")
    if r.returncode != 0:
        raise ACR1252EmulationError(f"pcscd {action} failed: {r.stderr.strip()[:150]}")


def _run(bench, args: str, timeout: int = 20) -> str:
    r = bench.run(f"python3 {HELPER_PATH} {args}", timeout=timeout)
    if r.returncode != 0:
        raise ACR1252EmulationError(
            f"acr_emu {args!r} rc={r.returncode}: {r.stdout.strip()[:120]} {r.stderr.strip()[:180]}")
    return r.stdout.strip()


def install_helper(bench) -> None:
    r = bench.run(
        f"mkdir -p {HELPER_DIR} && cat > {HELPER_PATH} <<'PYEOF'\n{HELPER}\nPYEOF")
    if r.returncode != 0:
        raise ACR1252EmulationError(f"helper install failed: {r.stderr[:150]}")


class Acr1252Emu:
    """Context: enter emulation (pcscd paused), write NDEF URI, exit.

    The pcscd pause window is the enter/exit exchange only — seconds —
    under the bench flock, matching the existing battery discipline.
    """

    def __init__(self, bench, ndef_uri: str | None = None, settle_s: float = 0.5):
        self.bench = bench
        self.ndef_uri = ndef_uri
        self.settle_s = settle_s

    def __enter__(self) -> "Acr1252Emu":
        install_helper(self.bench)
        _pcscd(self.bench, "stop")
        try:
            _run(self.bench, "enter")
            if self.ndef_uri:
                _run(self.bench, f"write {shlex.quote(ndef_uri_block_image(self.ndef_uri))}")
        finally:
            _pcscd(self.bench, "start")
        time.sleep(self.settle_s)
        return self

    def __exit__(self, *exc) -> None:
        _pcscd(self.bench, "stop")
        try:
            _run(self.bench, "exit")
        except ACR1252EmulationError:
            pass  # reader power-cycle also clears emulation
        finally:
            _pcscd(self.bench, "start")
