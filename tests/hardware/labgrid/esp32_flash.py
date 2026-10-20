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


def pcscd_lock_acquire(run=None, lock_path: str | None = None) -> int:
    """Hold the bench pcscd flock for the WHOLE flash window.

    Codex P1 on #299: stopping pcscd under a per-command flock releases
    the lock after each systemctl — another compliant bench process can
    lawfully restart pcscd mid-flash onto the reader port. The lock is
    held as an fd from quiesce until restore has restarted pcscd.
    (`lock_path=None` resolves the module constant at CALL time so
    tests can redirect it.)
    """
    import fcntl
    import os
    lp = lock_path or PCSCD_LOCK
    fd = os.open(lp, os.O_CREAT | os.O_RDWR)
    fcntl.flock(fd, fcntl.LOCK_EX)
    # bare systemctl on purpose: the fd above IS the lock — wrapping
    # this in a shell `flock <path>` opens a second file description
    # and self-deadlocks against it (flock(1) blocks on LOCK_EX)
    r = (run or _run_local)("systemctl stop pcscd.socket pcscd",
                            timeout=60)
    if r.returncode != 0:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)
        raise RuntimeError(
            f"pcscd stop failed (rc={r.returncode}) — the socket unit "
            "may still be armed and can re-grab the reader port "
            "mid-flash; refusing to flash (run with the rights the "
            f"bench expects). stderr: {(r.stderr or '')[-200:]}")
    return fd


def pcscd_lock_release(fd: int, run=None,
                       warn=lambda msg: print(f"# {msg}")
                       ) -> tuple[bool, None]:
    """Restart pcscd, then release the lock. Returns (pcscd_ok, None).

    A failed START does not fail the flash (the images are proven on
    flash) but is reported — the caller must know pcscd is down. The
    fd is unlocked/closed in a finally so a runner exception cannot
    leak the flock in a long-lived caller.
    """
    import fcntl
    import os
    pcscd_ok = True
    try:
        r = (run or _run_local)("systemctl start pcscd.socket pcscd",
                                timeout=60)
        if r.returncode != 0:
            pcscd_ok = False
            warn(f"pcscd RESTART FAILED (rc={r.returncode}) — other "
                 f"bench readers are DOWN until restarted manually. "
                 f"stderr: {(r.stderr or '')[-200:]}")
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)
    return pcscd_ok, None


def pcscd_stop(run=None) -> None:
    """Stop BOTH pcscd units under the bench flock (socket-activation!).

    Legacy helper — DOES NOT hold the lock across a flash window; the
    flash path uses pcscd_lock_acquire/release (Codex P1 on #299).
    """
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
                boot_check=None, capture=None, run=None, usb_reset=None,
                rebind=None, usb_path=None, quiesce=None, restore=None,
                quiesce_unit: str | None = None, wire_check=None,
                stopbits: int = 2
                ) -> tuple[bool, str]:
    """Flash an ESP32 with the full wedge discipline. Returns (ok, log).

    `images` maps flash offset -> image path (written in ONE esptool
    invocation, offsets sorted DESCENDING — the app lands before the
    partition table, so a mid-write failure leaves the OLD table
    standing; the most-structural element lands last).
    `erase_regions` are (start, len) regions erased AFTER all writes
    succeeded (otadata goes here — never erase before writing).
    `boot_check(text) -> bool` receives the post-reset console capture;
    the DEFAULT is `boot_check_no_conflict` — proof of boot is part of
    success, not an option (Codex P1 on #299; the dropped-reset class
    documented in AGENTS). Pass `boot_check=False` to explicitly opt
    out (logged loudly). Pass `boot_check="wire"` for console-none
    families (#313): instead of a console signature, one framed
    GetSlotStatus round-trip on the freshly flashed port (via
    gempc_smoke — any LRC-valid RDR reply proves the serve loop is
    alive; the CONFLICT veto is console-side and does not apply).
    `quiesce_unit` (#314): a systemd unit that owns the port (e.g. the
    nucula's standing `nucula-ccid-broker`) — stopped under the SAME
    held pcscd flock before the flash and restarted during restore.
    Killing its process with fuser is NOT enough: systemd would
    restart it onto the port mid-flash (the observed Checksum-error
    class). All side-effectful collaborators are injectable for the
    unit tests.
    """
    run = run or _run_local
    usb_reset = usb_reset or usbdevfs_reset
    rebind = rebind or ftdi_rebind
    capture = capture or _boot_capture
    log: list[str] = []
    lock_state = {"fd": None}

    if quiesce is None:
        def quiesce():
            lock_state["fd"] = pcscd_lock_acquire(run=run)
            if quiesce_unit:
                r = run(f"systemctl stop {shlex.quote(quiesce_unit)}",
                        timeout=60)
                log.append(f"$ systemctl stop {quiesce_unit} "
                           f"rc={r.returncode}")
                if r.returncode != 0:
                    raise RuntimeError(
                        f"unit stop failed: {quiesce_unit} (rc="
                        f"{r.returncode}) — it may still hold the port "
                        "mid-flash; refusing to flash. stderr: "
                        f"{(r.stderr or '')[-200:]}")
            kill_port_holders(port, run=run)
    if restore is None:
        def restore():
            set_ftdi_latency(port)
            if quiesce_unit:
                r = run(f"systemctl start {shlex.quote(quiesce_unit)}",
                        timeout=60)
                log.append(f"$ systemctl start {quiesce_unit} "
                           f"rc={r.returncode}")
                if r.returncode != 0:
                    log.append(f"# WARNING: {quiesce_unit} RESTART FAILED "
                               "(rc={r.returncode}) — the standing port "
                               "owner is DOWN; restart it manually")
            if lock_state["fd"] is not None:
                ok_pcscd, _ = pcscd_lock_release(lock_state["fd"],
                                                 run=run,
                                                 warn=log.append)
                if not ok_pcscd:
                    log.append("# WARNING carried in the verdict: pcscd "
                               "is DOWN after this flash")

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

    try:
        try:
            quiesce()
        except Exception as e:  # noqa: BLE001 — reported in the verdict
            log.append(f"# QUIESCE FAILED ({e}) — flash NOT attempted; "
                       "pcscd state unknown, verify before bench use")
            return False, "\n".join(log)
        state = {"written": False, "erased": False}
        # DESCENDING offset order: the app lands before the partition
        # table — a mid-write failure leaves the OLD table describing
        # flash (the most-structural element lands last).
        pairs = " ".join(
            f"0x{int(off, 0):x} {shlex.quote(str(img))}"
            for off, img in sorted(images.items(),
                                   key=lambda kv: -int(kv[0], 0)))
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
            log.append(
                "# ALL WRITES FAILED — flash contents may be PARTIALLY "
                "written (descending-offset order lands the app before "
                "the partition table: a failure during the table write "
                "leaves the OLD table, but the app region may hold a "
                "partial image). Do NOT assume the previous image "
                "boots; re-run this flash. If the port stays dead this "
                "is the physical-replug class (Gotcha #3): replug USB, "
                "then re-run.")
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

        if boot_check is not False:
            if boot_check == "wire":
                # #313: console-none proof-of-boot on the WIRE — one
                # framed GetSlotStatus round-trip; any LRC-valid RDR
                # reply proves the serve loop lives past the bootloader.
                check = wire_check or wire_boot_check
                ok_wire, detail = check(port)
                log.append(f"# wire boot check: {'passed' if ok_wire else 'FAILED'}"
                           f" — {detail}")
                if not ok_wire:
                    log.append("# BOOT CHECK FAILED (wire) — the freshly "
                               "flashed image does not answer GetSlotStatus; "
                               "flash verified ≠ boot verified.")
                    return False, "\n".join(log)
            else:
                check = boot_check if boot_check is not None \
                    else boot_check_no_conflict
                text = capture(port)
                if not check(text):
                    log.append("# BOOT CHECK FAILED — the console did not "
                               "prove a good boot (flash verified ≠ boot "
                               "verified; the dropped-reset class is "
                               "documented in AGENTS). Console tail:\n"
                               f"{text[-600:]}")
                    return False, "\n".join(log)
            log.append("# boot check passed")
        else:
            log.append("# BOOT CHECK SKIPPED (caller explicitly opted "
                       "out) — flash success is esptool-only, NOT "
                       "boot-proven")
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
    """The default boot proof: firmware-level evidence required.

    `Calling app_main()` is the IDF main_task line printed on every
    chip BEFORE the app body runs (our silencing happens inside
    app_main, so this line is the last console evidence of a good boot
    — bench-verified on c3). ROM/bootloader banners alone are NOT
    accepted: a boot-looping or aborting image replays them (Codex P1
    on #299); an empty capture fails — no evidence is not proof."""
    if "CONFLICT! driver_ng is not allowed" in text:
        return False
    return "Calling app_main" in text


def wire_boot_check(port: str, stopbits: int = 2,
                    settle_s: float = 2.0) -> tuple[bool, str]:
    """`--boot-check wire` (#313): console-none proof-of-boot on the wire.

    One framed GetSlotStatus round-trip on the freshly flashed port
    via gempc_smoke (safe opener: DTR/RTS deasserted before open).
    Any LRC-valid RDR reply passes — the serve loop lives past the
    bootloader. The CONFLICT veto is console-side and does not apply;
    "no reply" fails (no evidence is not proof). `settle_s` lets the
    freshly reset image reach its serve loop first.
    """
    import gempc_smoke
    import time as _time
    try:
        import serial  # noqa: F401 — presence probe: same env as probe
    except ImportError as e:
        return False, f"pyserial unavailable: {e}"
    _time.sleep(settle_s)
    try:
        ok, lines = gempc_smoke.probe(port, stopbits=stopbits)
        return ok, "; ".join(lines)
    except Exception as e:  # noqa: BLE001 — reported in the verdict
        return False, f"wire probe raised: {e!r}"


def _cli() -> int:
    """`python3 esp32_flash.py --port P --chip C --image OFF=PATH ...

    The build.sh --flash path routes through this CLI (Codex P1 on
    #299: the documented flashing flow must receive the same wedge
    discipline as the HIL harness). Repeatable flags: --image
    OFFSET=PATH, --erase START=LEN. Optional: --baud N (capped to the
    exporter-configured speed), --exporter-cfg PATH, --no-boot-check
    (explicit opt-out, logged), --boot-check wire (#313: console-none
    families — proof-of-boot is a GetSlotStatus round-trip, not a
    console signature), --quiesce-unit NAME (#314: a systemd unit that
    owns the port, e.g. nucula-ccid-broker — stopped/started under the
    held flock), --stopbits N (2 = GemPC serial, 1 = USB-CDC).
    """
    import argparse
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", required=True)
    ap.add_argument("--chip", required=True)
    ap.add_argument("--image", action="append", required=True,
                    metavar="OFFSET=PATH")
    ap.add_argument("--erase", action="append", default=[],
                    metavar="START=LEN")
    ap.add_argument("--baud", type=int, default=None)
    ap.add_argument("--exporter-cfg", default=None)
    ap.add_argument("--no-boot-check", action="store_true")
    ap.add_argument("--boot-check", choices=("console", "wire"),
                    default="console")
    ap.add_argument("--quiesce-unit", default=None)
    ap.add_argument("--stopbits", type=int, default=2)
    args = ap.parse_args()
    images = {}
    for spec in args.image:
        off, _, path = spec.partition("=")
        images[off] = path
    erases = tuple(
        (int(spec.split("=", 1)[0], 0), int(spec.split("=", 1)[1], 0))
        for spec in args.erase)
    if args.no_boot_check:
        boot_check = False
    elif args.boot_check == "wire":
        boot_check = "wire"
    else:
        boot_check = None
    ok, log = flash_esp32(
        args.port, args.chip, images, baud=args.baud,
        erase_regions=erases,
        exporter_cfg=Path(args.exporter_cfg) if args.exporter_cfg else None,
        boot_check=boot_check,
        quiesce_unit=args.quiesce_unit,
        wire_check=(lambda p: wire_boot_check(p, stopbits=args.stopbits))
        if boot_check == "wire" else None)
    print(log)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(_cli())
