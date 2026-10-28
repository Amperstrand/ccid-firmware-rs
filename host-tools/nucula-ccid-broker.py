#!/usr/bin/env python3
"""nucula-ccid-broker: relay the nucula's USB-CDC CCID to a PTY for pcscd.

WHY (issue #117): the ESP32-C3's USB-Serial/JTAG peripheral chip-resets
(rst:0x15) on the host DTR/RTS patterns that pcscd's libccidtwin applies
on every serial-reader open — and the C3 has no disable bit. A pcscd
session therefore boot-loops the reader.

HOW: this broker is the ONLY process that ever opens the CDC. It uses
the safe opener (dtr/rts deasserted BEFORE open — one rst per fresh
open is still expected on C3), settles through the boot, PROVES the
reader is serving (framed GetSlotStatus until an LRC-valid answer, the
issue #119 first-response ~1.3 s window), and only then exposes the PTY
symlink — pcscd's first handshake always lands on a serving reader.
It then holds the port open forever. pcscd's libccidtwin opens the PTY
symlink instead, where termios/TIOCMSET line control is a harmless
no-op. CCID bytes flow byte-transparent in both directions.

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

# GemPC Twin framing (ccid-transport-serial wire format) — only what the
# serving probe needs: LRC-validated GetSlotStatus request/response.
SYNC = 0x03
CTRL_ACK = 0x06
MSG_GET_SLOT_STATUS = 0x65


def lrc(data: bytes) -> int:
    x = 0
    for b in data:
        x ^= b
    return x


def gss_frame(seq: int) -> bytes:
    header = bytes([MSG_GET_SLOT_STATUS, 0, 0, 0, 0, 0, seq, 0, 0, 0])
    body = bytes([SYNC, CTRL_ACK]) + header
    return body + bytes([lrc(body)])


def first_rdr_frame(buf: bytes):
    """First structurally-valid RDR frame (mt >= 0x80) in `buf`, or None."""
    i = 0
    while i + 13 <= len(buf):
        if buf[i] != SYNC:
            i += 1
            continue
        dlen = int.from_bytes(buf[i + 3:i + 7], "little")
        total = 13 + dlen
        if total > len(buf) - i:
            i += 1
            continue
        if buf[i + 2] & 0x80 and lrc(buf[i:i + total - 1]) == buf[i + total - 1]:
            return buf[i:i + total]
        i += 1
    return None


def open_cdc() -> serial.Serial:
    port = BY_ID if os.path.exists(BY_ID) else "/dev/ttyACM0"
    s = serial.Serial()
    s.port = port
    s.baudrate = 115200
    s.timeout = 0.05
    s.write_timeout = 2.0  # bench rule: a wedged C3 CDC never drains TX —
    #                     # an unbounded write() blocks FOREVER (py-spy'd
    #                     # 2026-10-21: broker stuck in write() inside
    #                     # wait_serving while the board sat dead)
    s.dtr = False  # bench rule: deassert BEFORE open — no DTR pulse
    s.rts = False
    s.open()
    return s


def boot_settle(s: serial.Serial, cap_s: float = 6.0) -> None:
    """Drain the rst:0x15 boot cycle until the line goes idle (two empty
    reads, 1.5 s cap per the bench reader-timing rule), hard-capped at
    `cap_s`. Never fixed-size reads: inWaiting()-sized chunks only."""
    deadline = time.monotonic() + cap_s
    idle_deadline = time.monotonic() + 1.5
    while time.monotonic() < deadline:
        n = s.inWaiting()
        if n:
            s.read(n)
            idle_deadline = time.monotonic() + 1.5
        elif time.monotonic() > idle_deadline:
            return
        else:
            time.sleep(0.02)
    s.reset_input_buffer()  # cap hit: drop whatever is left


def wait_serving(s: serial.Serial, budget_s: float = 2.5) -> bool:
    """CDC-burst rule (issue #119): the first valid CCID response lands
    ~1.3 s after open. Poll framed GetSlotStatus until an LRC-valid RDR
    frame answers — the reader is provably serving BEFORE any pcscd
    byte is forwarded. Bonus: the probe GSS is the firmware's first
    CCID frame, which arms its clean-wire log ceiling."""
    seq = 0
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        seq = (seq % 255) + 1
        try:
            s.write(gss_frame(seq))
        except serial.SerialTimeoutException:
            # write_timeout tripped: TX is not draining (dead/wedged CDC).
            # Bounded failure, not a forever-block — let the caller's
            # retry ladder decide (bring_up_cdc reopens or reports).
            return False
        t0 = time.monotonic()
        buf = b""
        while time.monotonic() - t0 < 0.3:
            n = s.inWaiting()
            if n:
                buf += s.read(n)
                if first_rdr_frame(buf):
                    return True
            time.sleep(0.01)
    return False


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


def bring_up_cdc():
    """Open the CDC safely, settle the boot, prove serving. Returns the
    Serial or None (caller retries)."""
    try:
        s = open_cdc()
    except serial.SerialException as e:
        print(f"broker: CDC open failed ({e}) — retrying", flush=True)
        return None
    boot_settle(s)
    s.reset_input_buffer()  # boot banner must never reach pcscd
    if not wait_serving(s):
        print("broker: CDC opened but reader never served a framed GSS "
              "— dropping and retrying", flush=True)
        try:
            s.close()
        except Exception:
            pass
        return None
    return s


MSG_ICC_POWER_OFF = 0x63


def synth_power_off_response(host_frame: bytes) -> bytes:
    """Echo + SlotStatus ack for a suppressed IccPowerOff (issue #125
    shim): byte-identical to what the firmware answered for the real
    power-off on the bench (RDR_to_PC_SlotStatus, bStatus=0x01
    present+inactive)."""
    echo = host_frame
    seq = host_frame[8]
    body = bytes([SYNC, CTRL_ACK, 0x81, 0, 0, 0, 0, 0, seq, 0x01, 0x00, 0x00])
    return echo + body + bytes([lrc(body)])


def split_frames(buf: bytes):
    """Yield (start, end, type) for each complete GemPC frame in buf.

    #375: a truncated/garbage frame whose bytes land on the dwLength
    field (e.g. the 4-byte cut header `03 06 65 01` + the next probe's
    SYNC/CTRL) implies an insane length and used to `break` the scan
    WITHOUT rescanning — the shim then buffered every subsequent host
    frame forever and the reader looked dead (zero responses to any
    probe; misattributed to firmware). The firmware caps payloads at
    271 bytes, so any anchor implying more is garbage: skip ONE byte
    past it and rescan. Only a SANE-but-incomplete anchor is a genuine
    partial frame in flight worth waiting for.
    """
    SANE_MAX = 13 + 261  # SYNC+CTRL+10-byte header+LRC + payload cap;
                         # 261 = ccid-transport-serial MAX_CCID_PAYLOAD
                         # (271 is the full CCID message cap INCLUDING
                         # the 10-byte header — payload alone is 261)
    i = 0
    while i + 13 <= len(buf):
        if buf[i] != SYNC:
            i += 1
            continue
        dlen = int.from_bytes(buf[i + 3:i + 7], "little")
        total = 13 + dlen
        if total > SANE_MAX:
            i += 1  # garbage anchor: resync past it (#375)
            continue
        if total > len(buf) - i:
            break  # sane partial frame in flight
        yield i, i + total, buf[i + 2]
        i += total


class PowerOffShim:
    """IccPowerOff suppression (TEMPORARY firmware bridge, issue #125).

    The bench PN7160 firmware wedges presence permanently after a host
    IccPowerOff: the re-arm probe's RF_DISCOVER is rejected with NCI
    status 0xA0 (DISCOVERY_ALREADY_STARTED) and the deactivated ISO-DEP
    card sits in HALT, never re-activating — so pcscd's init sequence
    (power-on -> read ATR -> power-off) makes the card 'disappear' and
    EHStatusHandlerThread reports it removed forever (bench
    2026-10-11, wire-logged via this broker).

    Until the firmware fix lands, the broker answers IccPowerOff
    locally: the card physically stays powered, presence stays up, and
    the next session's IccPowerOn reuses the live activation. Deviation
    from CCID semantics: the card is not actually powered down between
    sessions (documented; remove with POWER_OFF_SHIM=0).
    """

    def __init__(self) -> None:
        self.enabled = os.environ.get("POWER_OFF_SHIM", "1") == "1"
        self.buf = bytearray()
        self.last_rx = time.monotonic()
        if self.enabled:
            print("broker: power-off shim ACTIVE (#125 bridge)", flush=True)

    def process(self, data: bytes) -> tuple[bytes, bytes]:
        """Buffer host bytes until frames complete. Returns
        (bytes_to_forward, bytes_to_answer_locally)."""
        # Stale-partial flush (bench 2026-10-28, conformance battery fuzz
        # "lone SYNC", deterministic 2/2): a stray SYNC buffered with no
        # follow-up fuses with the NEXT probe frame into a bogus
        # "sane-length complete" anchor at the stale byte — both frames
        # desync and the reader reads dead for seconds. Host frames never
        # pause mid-frame: bytes arriving after an idle gap are a NEW
        # frame, so a partial that survived the gap is garbage — drop it.
        now = time.monotonic()
        if self.buf and now - self.last_rx > 0.05:
            self.buf = bytearray()
        self.last_rx = now
        self.buf += data
        if not self.enabled:
            out, self.buf = bytes(self.buf), bytearray()
            return out, b""
        # Junk without a SYNC anchor would buffer forever: past one max
        # CCID frame of bytes with no complete frame, pass it through.
        if len(self.buf) > 13 + 65535:
            out, self.buf = bytes(self.buf), bytearray()
            return out, b""
        frames = list(split_frames(bytes(self.buf)))
        if not frames:
            return b"", b""
        end = frames[-1][1]
        chunk, self.buf = bytes(self.buf[:end]), bytearray(self.buf[end:])
        forward = bytearray()
        synth = bytearray()
        pos = 0
        for a, b, ftype in frames:
            forward += chunk[pos:a]
            if ftype == MSG_ICC_POWER_OFF:
                synth += synth_power_off_response(chunk[a:b])
                print("broker: power-off suppressed (shim)", flush=True)
            else:
                forward += chunk[a:b]
            pos = b
        forward += chunk[pos:]
        return bytes(forward), bytes(synth)


def tap(direction: str, data: bytes) -> None:
    """Journal wire traffic (both directions) for bench diagnostics —
    the broker is the single point where the whole pcscd exchange is
    visible."""
    if data:
        print(f"wire {direction}: {data.hex(' ')}", flush=True)


def main() -> int:
    # CDC comes up FIRST: the symlink is only created once the reader
    # has PROVEN it is serving, so a pcscd start can never race a
    # booting reader (CDC-burst rule #1, issue #119).
    s = None
    while s is None:
        s = bring_up_cdc()
        if s is None:
            time.sleep(2.0)

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
    print(f"broker: {s.port} <-> {LINK} -> {slave_path}", flush=True)
    shim = PowerOffShim()

    while True:  # CDC recovery loop: re-open, keep the SAME PTY alive
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
                tap("cdc->pty", data)
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
                    tap("pty->cdc", data)
                    forward, synth = shim.process(data)
                    if forward:
                        try:
                            os.write(s.fd, forward)
                        except OSError:
                            break
                    if synth:
                        os.write(master, synth)
        try:
            s.close()
        except Exception:
            pass
        print("broker: CDC dropped — recovering (PTY stays up)", flush=True)
        s = None
        while s is None:
            time.sleep(2.0)
            s = bring_up_cdc()
        import termios as t2
        t2.tcflush(master, t2.TCIOFLUSH)  # stale bytes must not reach pcscd


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        if os.path.islink(LINK):
            os.unlink(LINK)
        sys.exit(0)
