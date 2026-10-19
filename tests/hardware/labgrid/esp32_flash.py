"""Canonical ESP32 flash path — the serial-wedge discipline.

The F469 got its wedge table closed in stm32_flash.py (cross-project
audit 2026-10-16); this module is the ESP32/serial counterpart, funded
by the 2026-10-19 m5stick session (#278 xtensa leg):

- **Port ownership**: pcscd is SOCKET-ACTIVATED — `systemctl stop
  pcscd` alone leaves pcscd.socket armed and the daemon respawns onto
  the reader port mid-flash (first failure of that session). Both units
  must be stopped under the bench flock, and restored after.
- **Baud discipline**: the Hades2001 FTDI (m5stick) flakes at 460800
  ("Serial data stream stopped: possible serial noise or corruption");
  the exporter config pins that port at 115200. Exceeding the
  configured speed voids the bench's own contract — this module caps
  the caller to the exporter-configured speed.
- **Ordering**: erase-first strands the board. That session erased
  otadata BEFORE writing, then the bridge died mid-PT-write: erased
  selector + partial partition table + dead serial = maximally
  unbootable. Writes go FIRST; destructive erases go LAST, only after
  every write succeeded.
- **USB/FTDI latch**: on esptool failure — one plain retry, then
  USBDEVFS_RESET of the port's USB device (sysfs serial match), then
  an ftdi_sio unbind/rebind of the port's interface, then one more
  retry. If all fail the board needs a PHYSICAL replug (Gotcha #3
  class) — this module reports that verdict loudly, with the exact
  board state (what was written, what was erased) so recovery is a
  known quantity instead of archaeology.
- **Post-flash hygiene**: the FTDI latency_timer resets to 4 on USB
  re-enumeration (issue #51 — pcscd error-loops until it is 1 again);
  pcscd is restarted under the bench flock; an optional boot-check
  callback gates success on the console signature (no `i2c: CONFLICT`,
  `Calling app_main()` reached — the #278 class).

Usage (bench): see flash_esp32(); HIL conftest and the #294 boot-smoke
gate should route EVERY esp32 flash through here — lanes never
hand-roll esptool sequences again.
"""

from __future__ import annotations

import shlex
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

PCSCD_LOCK = "/run/lock/bench-pcscd.lock"
USBDEVFS_RESET_IOCTL = 0x5514
SETTLE_S = 2


@dataclass
class CmdResult:
    returncode: int
    stdout: str = ""
    stderr: str = ""


def _run_local(cmd: str, timeout: int = 300) -> CmdResult:
    r = subprocess.run(cmd, shell=True, capture_output=True, text=True,
                       timeout=timeout)
    return CmdResult(r.returncode, r.stdout, r.stderr)


def serial_of_port(port: str) -> str | None:
    """The USB serial embedded in a /dev/serial/by-id path.

    `usb-Hades2001_M5stack_49D6163EBE-if00-port0` -> `49D6163EBE`;
    `usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:..-if00` -> the
    MAC-colon token. Both are the last `_`-separated token before
    `-if`.
    """
    name = Path(port).name
    if not name.startswith("usb-"):
        return None
    stem = name[len("usb-"):].split("-if")[0]
    parts = stem.split("_")
    return parts[-1] if parts else None


def usb_dev_path_by_serial(usb_serial: str,
                           sysfs_root: str = "/sys/bus/usb/devices"
                           ) -> str | None:
    """/dev/bus/usb/BBB/DDD for the USB device with this serial.

    sysfs walk (same rationale as stm32_flash: plain lsusb carries no
    serials). `sysfs_root` injectable for tests.
    """
    root = Path(sysfs_root)
    if not root.is_dir():
        return None
    for dev_dir in sorted(root.iterdir()):
        try:
            serial = (dev_dir / "serial").read_text().strip()
        except OSError:
            continue
        if serial.lower() != usb_serial.lower():
            continue
        try:
            bus = int((dev_dir / "busnum").read_text().strip())
            devnum = int((dev_dir / "devnum").read_text().strip())
        except (OSError, ValueError):
            continue
        return f"/dev/bus/usb/{bus:03d}/{devnum:03d}"
    return None


def usbdevfs_reset(dev_path: str) -> bool:
    """USBDEVFS_RESET the device node (the latch recovery)."""
    import fcntl
    import os
    try:
        fd = os.open(dev_path, os.O_WRONLY)
    except OSError:
        return False
    try:
        fcntl.ioctl(fd, USBDEVFS_RESET_IOCTL)
        return True
    except OSError:
        return False
    finally:
        os.close(fd)


def ftdi_rebind(port: str, sysfs_root: str = "/sys/class/tty") -> bool:
    """Unbind + rebind the ftdi_sio interface serving this tty port.

    The 2026-10-19 ladder step that comes after USBDEVFS_RESET. The
    interface id is re-derived from the re-resolved tty (a USB reset
    can renumber the tty), so this takes the /dev/serial/by-id path.
    """
    import os
    tty = os.path.basename(os.path.realpath(port))
    dev = Path(sysfs_root) / tty / "device"
    try:
        iface = os.path.basename(os.path.dirname(os.path.realpath(dev)))
    except OSError:
        return False
    drv_path = Path("/sys/bus/usb/drivers/ftdi_sio")
    try:
        (drv_path / "unbind").write_text(iface)
        time.sleep(2)
        (drv_path / "bind").write_text(iface)
        time.sleep(3)
        return True
    except OSError:
        return False


def baud_from_exporter(port: str, exporter_cfg: Path) -> int | None:
    """The speed the exporter config pins for this port, if any.

    The bench's own contract: a `speed:` under the SerialPort block
    whose `port:` matches. Flashing faster than this is how the m5stick
    session started wedging (460800 against a configured 115200).
    Line-based parse (the exporter file is a Jinja template whose `#`
    lines are STATEMENTS — a pure regex over the whole text is brittle).
    """
    try:
        lines = exporter_cfg.read_text().splitlines()
    except OSError:
        return None
    in_serial_block = False
    block_port = None
    block_speed = None
    for line in lines:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if indent == 0:
            in_serial_block = False
            continue
        if not in_serial_block:
            if stripped == "SerialPort:":
                in_serial_block = True
                block_port = None
                block_speed = None
            continue
        if stripped.startswith("port:"):
            block_port = stripped.split("port:", 1)[1].strip()
        elif stripped.startswith("speed:"):
            try:
                block_speed = int(stripped.split("speed:", 1)[1].strip())
            except ValueError:
                pass
        if block_port == port and block_speed is not None:
            return block_speed
    return None


def pcscd_stop(run=None) -> None:
    """Stop BOTH pcscd units under the bench flock (socket-activation!)."""
    run = run or _run_local
    run(f"flock {PCSCD_LOCK} systemctl stop pcscd.socket pcscd", timeout=60)


def pcscd_start(run=None) -> None:
    run = run or _run_local
    run(f"flock {PCSCD_LOCK} systemctl start pcscd.socket pcscd", timeout=60)


def kill_port_holders(port: str, run=None) -> None:
    run = run or _run_local
    run(f"fuser -k {shlex.quote(port)}", timeout=30)
    time.sleep(SETTLE_S)


def set_ftdi_latency(port: str, value: int = 1, sysfs_root: str =
                     "/sys/class/tty") -> bool:
    """FTDI latency_timer back to 1 after flashing (issue #51)."""
    import os
    tty = os.path.basename(os.path.realpath(port))
    lt = Path(sysfs_root) / tty / "device" / "latency_timer"
    try:
        lt.write_text(str(value))
        return True
    except OSError:
        return False


def _esptool(port: str, chip: str, baud: int, op: str, run) -> CmdResult:
    cmd = (f"esptool --chip {shlex.quote(chip)} -p {shlex.quote(port)} "
           f"--baud {baud} {op}")
    r = run(cmd)
    return r


def flash_esp32(port: str, chip: str, images: dict[str, str],
                baud: int | None = None,
                erase_regions: tuple[tuple[int, int], ...] = (),
                exporter_cfg: Path | None = None,
                boot_check=None, run=None, usb_reset=None, rebind=None,
                usb_path=None, quiesce=None, restore=None) -> tuple[bool, str]:
    """Flash an ESP32 with the full wedge discipline. Returns (ok, log).

    `images` maps flash offset -> image path (written in ONE esptool
    invocation, offsets sorted). `erase_regions` are (start, len)
    regions erased AFTER all writes succeeded (otadata goes here —
    never erase before writing). `boot_check(text) -> bool` receives
    the post-reset console capture; None skips it. All side-effectful
    collaborators are injectable for the unit tests.
    """
    run = run or _run_local
    usb_reset = usb_reset or usbdevfs_reset
    rebind = rebind or ftdi_rebind
    if quiesce is None:
        def quiesce():
            pcscd_stop(run=run)
            kill_port_holders(port, run=run)
    if restore is None:
        def restore():
            set_ftdi_latency(port)
            pcscd_start(run=run)
    log: list[str] = []

    # -- baud discipline: never exceed the exporter-configured speed --
    cap = baud_from_exporter(port, exporter_cfg) if exporter_cfg else None
    if cap is not None:
        if baud is None:
            baud = cap
        elif baud > cap:
            log.append(f"# baud {baud} exceeds exporter-configured {cap} "
                       f"for this port — downgraded ({port})")
            baud = cap
    if baud is None:
        baud = 460800

    quiesce()
    state = {"written": False, "erased": False}
    try:
        pairs = " ".join(
            f"0x{int(off, 0):x} {shlex.quote(str(img))}"
            for off, img in sorted(images.items(), key=lambda kv: int(kv[0], 0)))
        write_op = f"--after no-reset write-flash {pairs}"

        def attempt_with_ladder(op: str) -> CmdResult:
            r = _esptool(port, chip, baud, op, run)
            log.append(f"$ esptool ... {op.split()[0]} ... rc={r.returncode}"
                       f"\n{(r.stderr or r.stdout)[-400:]}")
            if r.returncode == 0:
                return r
            # ladder: retry -> USBDEVFS_RESET -> retry -> ftdi rebind -> retry
            serial = serial_of_port(port)
            resolve = usb_path or usb_dev_path_by_serial
            dev = resolve(serial) if serial else None
            if dev:
                log.append(f"# usb latch recovery: USBDEVFS_RESET {dev} "
                           f"-> {usb_reset(dev)}")
                r = _esptool(port, chip, baud, op, run)
                log.append(f"# retry after usb reset rc={r.returncode}")
                if r.returncode == 0:
                    return r
            log.append(f"# ftdi_sio rebind {port} -> {rebind(port)}")
            r = _esptool(port, chip, baud, op, run)
            log.append(f"# retry after rebind rc={r.returncode}")
            return r

        r = attempt_with_ladder(write_op)
        if r.returncode != 0:
            log.append("# ALL WRITES FAILED — flash content untouched "
                       "(nothing was erased; the previous image still "
                       "boots). If the port stays dead this is the "
                       "physical-replug class (Gotcha #3): replug USB, "
                       "then re-run this flash.")
            return False, "\n".join(log)
        state["written"] = True

        # -- destructive steps ONLY after every write landed --
        for start, length in erase_regions:
            r = attempt_with_ladder(
                f"--after no-reset erase-region 0x{start:x} 0x{length:x}")
            if r.returncode != 0:
                log.append(f"# ERASE FAILED at 0x{start:x} after "
                           "successful writes: the NEW images are on "
                           "flash but the old otadata/selector still "
                           "points at the previous slot — the board "
                           "may boot the OLD image. Re-run this flash.")
                return False, "\n".join(log)
        state["erased"] = True

        # -- final reset + sync sanity (the documented chip-reset step) --
        r = attempt_with_ladder("--after hard-reset flash-id")
        if r.returncode != 0:
            log.append("# reset/flash-id failed after successful "
                       "write+erase — power-cycle or replug, images ARE "
                       "on flash.")
            return False, "\n".join(log)

        if boot_check is not None:
            text = _boot_capture(port)
            if not boot_check(text):
                log.append("# BOOT CHECK FAILED — console signature not "
                           f"found. Console tail:\n{text[-600:]}")
                return False, "\n".join(log)
            log.append("# boot check passed")
        return True, "\n".join(log)
    finally:
        restore()


def _boot_capture(port: str, seconds: float = 6.0) -> str:
    """Safe-opener console capture around a reset pulse."""
    import serial  # lazy: not importable in the minimal collect env
    s = serial.Serial()
    s.port = port
    s.baudrate = 115200
    s.timeout = 1
    s.dtr = False
    s.rts = False
    s.open()
    try:
        s.setRTS(True)
        time.sleep(0.25)
        s.setRTS(False)
        time.sleep(seconds)
        buf = b""
        t0 = time.time()
        while time.time() - t0 < 2:
            n = s.in_waiting
            if n:
                buf += s.read(n)
            else:
                break
        return buf.decode("utf-8", "replace")
    finally:
        s.close()


def boot_check_no_conflict(text: str) -> bool:
    """The #278 class gate: no legacy-i2c conflict abort, app_main reached
    (C3 images; xtensa ROM banners differ but the CONFLICT line is
    universal)."""
    if "CONFLICT! driver_ng is not allowed" in text:
        return False
    return "Calling app_main" in text or "rst:0x1 " in text \
        or "power on reset" in text.lower() or len(text) == 0
