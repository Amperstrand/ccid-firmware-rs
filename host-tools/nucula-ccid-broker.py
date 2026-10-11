#!/usr/bin/env python3
"""nucula-ccid-broker: relay the nucula's USB-CDC CCID to a PTY for pcscd.

WHY (issue #117): the ESP32-C3's USB-Serial/JTAG peripheral chip-resets
(rst:0x15) on the host DTR/RTS patterns that pcscd's libccidtwin applies
on every serial-reader open — and the C3 has no disable bit. A pcscd
session therefore boot-loops the reader.

HOW: this broker is the ONLY process that ever opens the CDC. It uses
the safe opener (dtr/rts deasserted BEFORE open — one rst per fresh
open is still expected on C3), settles through the boot, purges the
banner, and then holds the port open forever. pcscd's libccidtwin opens
the PTY symlink instead, where termios/TIOCMSET line control is a
harmless no-op. CCID bytes flow byte-transparent in both directions.

USAGE:
  python3 nucula-ccid-broker.py            # foreground
  # systemd: see nucula-ccid-broker.service (Unit file beside this)

reader.conf entry (colons in by-id paths break DEVICENAME; the PTY path
has none):
  DEVICENAME  /dev/nucula-ccid:GemPCTwin
  FRIENDLYNAME "Nucula CCID"
  LIBPATH     /usr/lib/pcsc/drivers/serial/libccidtwin.so
"""
import os
import pty
import select
import serial
import sys
import time

BY_ID = ("/dev/serial/by-id/"
         "usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:72:9A:50:18-if00")
LINK = "/dev/nucula-ccid"
SETTLE_S = 6.0  # one rst:0x15 boot cycle per fresh open (C3 behavior)


def open_cdc() -> serial.Serial:
    port = BY_ID if os.path.exists(BY_ID) else "/dev/ttyACM0"
    s = serial.Serial()
    s.port = port
    s.baudrate = 115200
    s.timeout = 0.05
    s.dtr = False  # bench rule: deassert BEFORE open — no DTR pulse
    s.rts = False
    s.open()
    return s


def pty_raw(fd: int) -> None:
    import termios
    attr = termios.tcgetattr(fd)
    # iflag: off ICRNL/IXON/INLCR/ISTRIP; oflag: off OPOST/ONLCR;
    # lflag: off ECHO/ICANON/ISIG/IEXTEN — byte transparency for
    # pcscd + no echo of its own frames back at it
    attr[0] &= ~(termios.ICRNL | termios.IXON | termios.INLCR | termios.ISTRIP | termios.INPCK)
    attr[1] &= ~(termios.OPOST | termios.ONLCR)
    attr[3] &= ~(termios.ECHO | termios.ICANON | termios.ISIG | termios.IEXTEN)
    attr[6][termios.VMIN] = 1
    attr[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, attr)


def purge_stale(master: int, slave_path: str) -> None:
    """Flush both directions between pcscd sessions.

    libccidtwin never resyncs: stale response bytes from a dead pcscd
    session are read first by the next one and desync it permanently
    (bench 2026-10-11: ReadSerial 'Got 0x63/0x64...' then error-loop).
    Called when the master reports EIO — i.e. NO slave is open.
    """
    import termios as t
    try:
        fd = os.open(slave_path, os.O_RDWR | os.O_NOCTTY)
        t.tcflush(fd, t.TCIOFLUSH)
        os.close(fd)
    except OSError:
        pass
    t.tcflush(master, t.TCIOFLUSH)


def main() -> int:
    master, slave = pty.openpty()
    pty_raw(slave)
    import termios as t
    t.tcflush(slave, t.TCIOFLUSH)
    slave_path = os.ttyname(slave)
    os.chmod(slave_path, 0o666)
    if os.path.islink(LINK) or os.path.exists(LINK):
        os.unlink(LINK)
    os.symlink(slave_path, LINK)
    # Do NOT keep the slave open: with no slave fd held, a master read
    # returns EIO exactly while pcscd has the port closed — the trigger
    # for purge_stale(). Keeping it open would let stale bytes survive
    # pcscd restarts.
    os.close(slave)

    while True:  # CDC recovery loop: re-open, keep the SAME PTY alive
        try:
            s = open_cdc()
        except serial.SerialException as e:
            print(f"broker: CDC open failed ({e}) — retrying", flush=True)
            time.sleep(2.0)
            continue
        time.sleep(SETTLE_S)
        s.reset_input_buffer()  # boot banner must never reach pcscd
        import termios as t
        t.tcflush(master, t.TCIOFLUSH)  # stale bytes must not reach pcscd
        print(f"broker: {s.port} <-> {LINK} -> {slave_path}", flush=True)
        fds = [s.fd, master]
        while True:
            try:
                r, _, _ = select.select(fds, [], [], 5.0)
            except InterruptedError:
                continue
            if s.fd in r:
                try:
                    data = os.read(s.fd, 4096)
                except OSError:
                    break
                if not data:
                    break  # CDC gone (reset by another process / unplug)
                os.write(master, data)
            if master in r:
                try:
                    data = os.read(master, 4096)
                except OSError:
                    # No slave open (pcscd closed / between sessions):
                    # purge stale bytes so the next session starts clean.
                    purge_stale(master, slave_path)
                    time.sleep(0.2)
                    continue
                if data:
                    try:
                        os.write(s.fd, data)
                    except OSError:
                        break
        try:
            s.close()
        except Exception:
            pass
        print("broker: CDC dropped — recovering (PTY stays up)", flush=True)
        time.sleep(2.0)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        if os.path.islink(LINK):
            os.unlink(LINK)
        sys.exit(0)


if __name__ == "__main__":
    sys.exit(main())
