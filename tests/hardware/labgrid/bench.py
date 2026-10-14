#!/usr/bin/env python3
"""Bench coordination for CCID HIL tests (#65): places, flock, ledger.

Three exclusivity layers, taken in this order and never reversed
(AB-BA — same protocol as bolty-rs tools/hil, documented in
docs/labgrid-bench-sharing.md):

1. BenchLock flock on /tmp/amperstrand-bench.lock — the only layer that
   excludes same-user sessions of OTHER projects on the shared bench
   (bolty-rs, micronuts): labgrid places only exclude per-place.
2. labgrid place acquisition — excludes other labgrid sessions per-place.
3. Tests that only read pcscd state need neither, but MUST NOT run while
   another holder toggles pcscd (the battery stops/starts it).

Run ledger: every session appends one line to tests/hardware/labgrid/
results/history.jsonl (OpenHTF pattern, borrowed from bolty-rs).
"""

from __future__ import annotations

import fcntl
import json
import os
import subprocess
import sys
import time
from contextlib import contextmanager
from pathlib import Path

RESULTS_DIR = Path(__file__).parent / "results"
LEDGER_PATH = RESULTS_DIR / "history.jsonl"
BENCH_LOCK_PATH = Path("/tmp/amperstrand-bench.lock")

# The bench's five exported places (exporter-ai-legion-nfc.yaml).
PLACES = ("stm32-ccid", "nucula-c3", "m5stick", "ref-acr1252", "ref-cardman")

DUT_PLACES = ("stm32-ccid", "nucula-c3", "m5stick")
REFERENCE_PLACES = ("ref-acr1252", "ref-cardman")


def lg(place: str, *args: str, timeout: int = 30) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["labgrid-client", "-p", place, *args],
        capture_output=True, text=True, timeout=timeout, check=False,
    )


def place_holder(place: str) -> str | None:
    """Current holder of a place ("" = free, None = coordinator error).

    `labgrid-client who` prints the GLOBAL acquisition table (the -p
    filter does not apply to the output) — match rows by Place column.
    """
    r = lg(place, "who")
    if r.returncode != 0:
        return None
    for line in r.stdout.strip().splitlines()[1:]:
        parts = line.split()
        if len(parts) >= 4 and parts[2] == place:
            return f"{parts[0]}@{parts[1]}"
    return ""


@contextmanager
def bench_lock(timeout_s: float = 0.0):
    """Cross-project bench flock. Non-blocking by default: a held lock
    raises BenchLockHeld immediately (caller decides to skip or wait)."""
    BENCH_LOCK_PATH.touch(exist_ok=True)
    fd = os.open(BENCH_LOCK_PATH, os.O_RDWR)
    deadline = time.monotonic() + timeout_s
    while True:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            break
        except BlockingIOError:
            if time.monotonic() >= deadline:
                os.close(fd)
                raise BenchLockHeld(
                    "bench flock held by another session (bolty-rs / "
                    "micronuts / ccid): /tmp/amperstrand-bench.lock"
                )
            time.sleep(1.0)
    try:
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


class BenchLockHeld(Exception):
    pass


@contextmanager
def acquired_place(place: str, poll_s: float = 0.0):
    """Acquire a labgrid place; raise PlaceBusy (with holder) if held."""
    r = lg(place, "acquire")
    if r.returncode != 0:
        raise PlaceBusy(place, place_holder(place), r.stderr.strip())
    try:
        yield place
    finally:
        lg(place, "release")


class PlaceBusy(Exception):
    def __init__(self, place: str, holder: str | None, detail: str = ""):
        self.place = place
        self.holder = holder
        super().__init__(
            f"labgrid place '{place}' held by "
            f"{holder or 'unknown session'} {detail}".strip()
        )


# F469 ST-LINK serial, pinned by the exporter config (flashed_firmware
# resolves it from there too — one place to be right).
F469_STLINK_SERIAL = "066FFF515786534867184152"


def dut_absent_reason(place: str) -> str | None:
    """Why this place's DUT is detectably not testable (None = testable).

    SKIP≠FAIL doctrine (#183): hardware that is physically absent, or
    deliberately serving a different firmware variant, says nothing
    about the code under test — those legs SKIP with this reason. Only
    present-and-expected hardware may FAIL.
    """
    try:
        by_id = sorted(os.listdir("/dev/serial/by-id"))
    except OSError:
        by_id = []
    lsusb = subprocess.run(["lsusb"], capture_output=True, text=True).stdout

    if place == "stm32-ccid":
        stlink = any(F469_STLINK_SERIAL in p for p in by_id)
        cherry = "046a:003e" in lsusb
        if not stlink and not cherry:
            return ("F469 physically detached — its ST-LINK ("
                    f"{F469_STLINK_SERIAL[:8]}…) is not on USB and no "
                    "Cherry DUT enumerates; nothing to test")
    elif place == "m5stick":
        # Primary signal: the m5stick's vpcd reader live in pcscd (slot
        # "00 01" is its listener, tools/hil §6) — robust even when the
        # vpcd firmware is quiet (connected == silent). Fallback: the
        # GemPC wire is SILENT by clean-wire doctrine while the wifi-vpcd
        # build streams a console on UART0, so printable noise from a
        # short open also means wrong variant.
        try:
            from smartcard.System import readers  # noqa: PLC0415

            if any("Nucula VPCD 00 01" in str(r) for r in readers()):
                return ("m5stick is serving pcscd through its live vpcd "
                        "reader (Nucula VPCD 00 01) — the wifi-vpcd "
                        "firmware variant; GemPC legs skip until the "
                        "standard image returns")
        except Exception:
            pass
        try:
            import serial as pyserial  # noqa: PLC0415
        except ImportError:
            return None
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from serial_ports import resolve  # noqa: PLC0415

        try:
            port = resolve("m5stick")
        except Exception:
            return None
        try:
            with pyserial.Serial(port, 115200, timeout=0.2) as s:
                s.setDTR(False)
                s.setRTS(False)
                buf = b""
                deadline = time.time() + 1.5
                while time.time() < deadline:
                    n = s.in_waiting
                    if n:
                        buf += s.read(n)
                    else:
                        time.sleep(0.1)
        except OSError:
            return None
        printable = sum(1 for b in buf if 32 <= b < 127 or b in (9, 10, 13))
        if len(buf) > 20 and printable >= len(buf) * 0.8:
            return ("m5stick is streaming on UART0 — the wifi-vpcd console "
                    "variant is flashed (another lane's active firmware); "
                    "GemPC legs skip until the standard image returns")
    return None


def ledger(event: str, **fields) -> None:
    """Append one run-record line. Never raises: a ledger failure must
    not fail a bench run."""
    try:
        RESULTS_DIR.mkdir(parents=True, exist_ok=True)
        record = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "event": event}
        record.update(fields)
        with LEDGER_PATH.open("a") as f:
            f.write(json.dumps(record) + "\n")
    except OSError:
        pass


def free_places(candidates: tuple[str, ...] = PLACES) -> dict[str, str | None]:
    """Place -> holder mapping for a quick bench overview ('' = free)."""
    out: dict[str, str | None] = {}
    for p in candidates:
        holder = place_holder(p)
        out[p] = holder
    return out
