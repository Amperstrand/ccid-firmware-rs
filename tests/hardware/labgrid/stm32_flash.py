"""Canonical F469 flash path — the shared-board wedge discipline.

Cross-project audit (2026-10-16): the STM32F469I-DISCO is flashed by
three Amperstrand projects (ccid Cherry reader, micronuts wallet,
bolty/gm65 scanner), and each independently documented HALF of the
wedge pair this module closes:

- bolty-rs docs/labgrid-bench-sharing.md #2: "st-flash write alone
  wedges the target's USB -> every write is followed by
  --connect-under-reset reset" (SYSRESETREQ leaves the USB peripheral
  stale; only ccid firmware carries the in-firmware PHY reset that
  survives a plain write — foreign firmware does not).
- micronuts firmware/AGENTS.md: "When the firmware is running with USB
  CDC active, the STM32F469 can lock out SWD (SwdDpWait errors)" —
  a plain connect cannot even attach; --connect-under-reset can.
- ccid AGENTS (.208 bench delta): the ST-LINK dongle itself latches —
  recovered by USBDEVFS_RESET (ioctl 0x5514) on its device node.

So every flash goes: connect-under-reset WRITE, connect-under-reset
RESET (belt+braces), and on a connect failure one USBDEVFS_RESET of the
pinned ST-LINK + a single retry. The ST-LINK stays pinned by serial
(the 2026-10-14 board-crossing incident: an F469 image landed on the
F746 via whoever's adapter was attached).
"""

from __future__ import annotations

import re
import shlex
import subprocess
from dataclasses import dataclass
from pathlib import Path

DEFAULT_FLASH_BASE = "0x08000000"
STLINK_USB_RESET_IOCTL = 0x5514  # USBDEVFS_RESET


@dataclass
class CmdResult:
    returncode: int
    stdout: str = ""
    stderr: str = ""


def _run_local(cmd: str, timeout: int = 60) -> CmdResult:
    r = subprocess.run(cmd, shell=True, capture_output=True, text=True,
                       timeout=timeout)
    return CmdResult(r.returncode, r.stdout, r.stderr)


def stlink_usb_path(stlink_serial: str,
                    sysfs_root: str = "/sys/bus/usb/devices") -> str | None:
    """/dev/bus/usb/BBB/DDD for the USB device with this serial, or None.

    sysfs is the reliable source: plain `lsusb` output does NOT include
    device serials (only -v descriptors do), so an lsusb-text match can
    never succeed on a real host (Codex P1 on #216). Each device dir
    exposes `serial`, `busnum` and `devnum` files. `sysfs_root` is
    injectable for tests.
    """
    root = Path(sysfs_root)
    if not root.is_dir():
        return None
    for dev_dir in sorted(root.iterdir()):
        serial_file = dev_dir / "serial"
        try:
            serial = serial_file.read_text().strip()
        except OSError:
            continue
        if serial.lower() != stlink_serial.lower():
            continue
        try:
            bus = int((dev_dir / "busnum").read_text().strip())
            devnum = int((dev_dir / "devnum").read_text().strip())
        except (OSError, ValueError):
            continue
        return f"/dev/bus/usb/{bus:03d}/{devnum:03d}"
    return None


def usbdevfs_reset(dev_path: str) -> bool:
    """USBDEVFS_RESET the ST-LINK node (the .208 latch recovery)."""
    import fcntl
    import os
    try:
        fd = os.open(dev_path, os.O_WRONLY)
    except OSError:
        return False
    try:
        fcntl.ioctl(fd, STLINK_USB_RESET_IOCTL)
        return True
    except OSError:
        return False
    finally:
        os.close(fd)


def flash_f469(bin_path: str, stlink_serial: str, run=None, reset=None,
               flash_base: str = DEFAULT_FLASH_BASE) -> tuple[bool, str]:
    """Flash + unwedge. Returns (ok, log).

    `run`/`reset` are injectable for tests (defaults: local subprocess
    for st-flash, direct ioctl for the USB reset).
    """
    run = run or _run_local
    reset = reset or usbdevfs_reset
    log: list[str] = []

    def attempt(tag: str) -> CmdResult:
        quoted_bin = shlex.quote(str(bin_path))
        cmd = (f"st-flash --connect-under-reset --serial {stlink_serial} "
               f"{tag} {quoted_bin} {flash_base}" if tag == "write" else
               f"st-flash --connect-under-reset --serial {stlink_serial} reset")
        r = run(cmd, timeout=60)
        log.append(f"$ {cmd}\nrc={r.returncode}\n{r.stdout}{r.stderr}")
        return r

    write = attempt("write")
    if write.returncode != 0:
        # ST-LINK latch class: one USBDEVFS_RESET + one retry before
        # giving up (the .208 lesson — never a bare second attempt).
        dev = stlink_usb_path(stlink_serial)
        if dev:
            log.append(f"# st-link latch recovery: USBDEVFS_RESET {dev} "
                       f"-> {reset(dev)}")
            write = attempt("write")
        if write.returncode != 0:
            return False, "\n".join(log)

    # Post-write reset (the gm65 lesson): the write's own reset is a
    # SYSRESETREQ; a connect-under-reset reset is the proven unwedge.
    reset_result = attempt("reset")
    if reset_result.returncode != 0:
        return False, "\n".join(log)
    return True, "\n".join(log)


def resolve_stlink_serial(exporter_cfg: Path) -> str:
    """The F469 ST-LINK serial pinned by the in-repo exporter config.

    Single source of truth — bare local st-flash targets whatever
    ST-LINK is attached (board-crossing hazard, 2026-10-14).
    """
    text = exporter_cfg.read_text()
    m = re.search(
        r"stm32f469-stlink:.*?usb-STMicroelectronics_STM32_STLink_([0-9A-Fa-f]+)-if02",
        text, re.DOTALL)
    if not m:
        raise ValueError(
            f"cannot resolve the F469 ST-LINK serial from {exporter_cfg} — "
            "refusing to flash an unpinned adapter (board-crossing hazard)")
    return m.group(1)
