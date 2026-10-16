"""ACR1252 card-emulation fixture (NFC Forum Type-2 / MIFARE Ultralight).

STATUS (updated 2026-10-16 after the #207 merge + first tier-1 run on
hardware): tier-1 PASSED at the TRANSPORT level — the corrected
ten-byte CCID header changed behavior (no framing errors), enter/exit
exchanges complete without error under the pcscd flock, place-acquired.
CAVEAT (Codex P2 on #213): `exit` discards the reply and `enter`
tolerates SENT-NO-REPLY, so the pass does NOT prove emulation ENGAGED
— only that the transport is sound. The NDEF WRITE path returns no
reply within 3 s (`write not confirmed: no-reply`); with engagement
unconfirmed, the suspect list is BOTH the write format AND the enter
command itself. History (overnight 2026-10-16 session, salvaged +
review-fixed via #207): pcscd cannot reach the escape interface (no
FEATURE_CCID_ESC advertised); the raw-USB PICC transport with pcscd
paused works.

NEXT SESSION (verify in this order):
0. Close the engagement question FIRST: make the helper's exit path
   surface its reply (it currently discards it — Codex P2 on #213) and
   use the §5.6.2 Read Card Emulation Data probe to prove emulation is
   active before touching the write format.
1. ACS API §5.6: the write escape's exact APDU layout (block address
   units, max data per command — the current single-shot 28-byte NDEF
   image write may exceed it).
2. RF coupling: confirm the ACR1252 antenna is physically within range
   of the nucula PN7160 and the m5stick RC522 (user reports shared
   field; unverified remotely).
3. Enter emulation, restart pcscd, watch `SCardGetStatusChange` on the
   serving readers for card arrival.
4. If still dark: try the ACS Android-library byte order for the enter
   command, and the §5.6.2 Read Card Emulation Data escape as a
   control-plane probe (does not need RF).
5. ISO-DEP (Type-4 / NTAG424) flows are NOT emulatable on this
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

import contextlib
import fcntl
import os
import shlex
import subprocess
import time

HELPER_DIR = f"/tmp/hil-ccid-bench-{os.getuid()}"
HELPER_PATH = f"{HELPER_DIR}/acr_emu.py"
# pcscd serialization (AGENTS.md bench rules): every stop/start window
# holds THIS lock, so compliant probes/batteries never see pcscd flap
# under them.
PCSCD_LOCK = "/run/lock/bench-pcscd.lock"


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
    # Full TEN-byte CCID header (Codex P1 on #207): type + dwLength(LE)
    # + bSlot + bSeq + bBWI/specific + RFU(2) — same layout as
    # board_identity.escape_frame's raw header. The salvaged version
    # emitted nine bytes, so the first payload byte (E0) was absorbed as
    # the last header byte and every command was misaligned.
    msg = (bytes([0x6B]) + len(data).to_bytes(4, "little")
           + bytes([0x00, _seq, 0x00, 0x00, 0x00]) + data)
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


def _run(args: str, timeout: int = 20) -> str:
    r = subprocess.run(f"python3 {HELPER_PATH} {args}", shell=True,
                       capture_output=True, text=True, timeout=timeout)
    if r.returncode != 0:
        raise ACR1252EmulationError(
            f"acr_emu {args!r} rc={r.returncode}: {r.stdout.strip()[:120]} {r.stderr.strip()[:180]}")
    return r.stdout.strip()


def install_helper() -> None:
    r = subprocess.run(
        f"mkdir -p {HELPER_DIR} && cat > {HELPER_PATH} <<'PYEOF'\n{HELPER}\nPYEOF",
        shell=True, capture_output=True, text=True)
    if r.returncode != 0:
        raise ACR1252EmulationError(f"helper install failed: {r.stderr[:150]}")


def _systemctl(action: str) -> None:
    r = subprocess.run(["sudo", "-n", "systemctl", action,
                        "pcscd.socket", "pcscd.service"],
                       capture_output=True, text=True)
    if r.returncode != 0:
        raise ACR1252EmulationError(f"pcscd {action} failed: {r.stderr.strip()[:150]}")


@contextlib.contextmanager
def pcscd_locked_paused():
    """pcscd paused for the window, holding /run/lock/bench-pcscd.lock
    across the WHOLE stop→operation→start span (Codex P1 on #207: a
    bare toggle can race any compliant lock-holding probe/battery).
    Start polls for readiness like test_bench_state._pcscd — rapid
    toggles need longer than a fixed sleep (bench-observed flake).
    """
    fd = os.open(PCSCD_LOCK, os.O_CREAT | os.O_WRONLY)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        _systemctl("stop")
        time.sleep(1.0)
        try:
            yield
        finally:
            _systemctl("start")
            for _ in range(20):
                r = subprocess.run(["systemctl", "is-active", "pcscd.service"],
                                   capture_output=True, text=True)
                if r.returncode == 0 and r.stdout.strip() == "active":
                    break
                time.sleep(0.5)
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


class Acr1252Emu:
    """Context: enter emulation (pcscd paused under the pcscd flock),
    write NDEF URI, exit. The pcscd pause window is the enter/exit
    exchange only — seconds — under the bench flock, matching the
    existing battery discipline.
    """

    def __init__(self, ndef_uri: str | None = None, settle_s: float = 0.5):
        self.ndef_uri = ndef_uri
        self.settle_s = settle_s

    def __enter__(self) -> "Acr1252Emu":
        install_helper()
        with pcscd_locked_paused():
            _run("enter")
            if self.ndef_uri:
                _run(f"write {shlex.quote(ndef_uri_block_image(self.ndef_uri))}")
        time.sleep(self.settle_s)
        return self

    def __exit__(self, *exc) -> None:
        with pcscd_locked_paused():
            try:
                _run("exit")
            except ACR1252EmulationError:
                pass  # reader power-cycle also clears emulation
