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
from contextlib import ExitStack, contextmanager
from pathlib import Path

RESULTS_DIR = Path(__file__).parent / "results"
LEDGER_PATH = RESULTS_DIR / "history.jsonl"
BENCH_LOCK_PATH = Path("/tmp/amperstrand-bench.lock")

# The bench's exported places (exporter-ai-legion-nfc.yaml).
#
# ref-acr1252 RETIRED from this bench 2026-10-25: the reader moved to
# ai-legion-small (place ref-acr1252-small, conwrt-bench consumption —
# retirement recorded on that place's coordinator note, #71). Nothing on
# .208 serves an ACR1252 to pcscd anymore, so ccid legs can neither
# acquire the old place nor probe its identity.
PLACES = ("stm32-ccid", "nucula-c3", "m5stick", "ref-cardman")

DUT_PLACES = ("stm32-ccid", "nucula-c3", "m5stick")
# ref-cardman only — the ACR1252 reference now lives on ai-legion-small
# (see PLACES note).
REFERENCE_PLACES = ("ref-cardman",)


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


def bench_lock_mode(skip_labgrid: bool) -> str:
    """#334: what --skip-labgrid skips, in one testable place.

    The flag is the documented coordinator-outage escape hatch: it
    skips PLACE acquisition only. The cross-project flock
    (/tmp/amperstrand-bench.lock) does not depend on the coordinator —
    pcscd/serial legs must still exclude bolty-rs/micronuts sessions,
    so the lock mode is never "none".
    """
    return "flock-only" if skip_labgrid else "flock+places"


def yield_acquired_place(place: str, skip, ledger_fn=None):
    """#334: acquire `place` for a fixture body; SKIP on the acquire race.

    `acquired_place` RAISES PlaceBusy when another session wins the
    place between the holder check and the acquire — with no caller
    catching it, the promised busy-place skip surfaced as a setup
    ERROR. This wrapper turns that raise into `skip(reason)` (with the
    winning holder named) and releases the place on teardown. `skip` is
    injected (bench.py stays pytest-free); the fixture passes
    `pytest.skip`.
    """
    ledger_fn = ledger_fn or ledger
    with ExitStack() as stack:
        try:
            p = stack.enter_context(acquired_place(place))
        except PlaceBusy as e:
            skip(f"place busy (lost the check→acquire race): {e}")
            return
        ledger_fn("place_acquired", place=p)
        yield p
    ledger_fn("place_released", place=place)


# F469 ST-LINK serial, pinned by the exporter config (flashed_firmware
# resolves it from there too — one place to be right).
F469_STLINK_SERIAL = "066FFF515786534867184152"
F469_ISERIAL = "ST2XXX-001"


def lsusb_serials(vidpid: str) -> str:
    """Concatenated iSerial strings of the given VID:PID device(s) —
    cheap enough for one targeted `-d` query (vs full lsusb -v)."""
    try:
        out = subprocess.run(["lsusb", "-d", vidpid, "-v"],
                             capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return " ".join(line.split()[-1] for line in out.stdout.splitlines()
                    if line.strip().startswith("iSerial"))


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

    if place == "stm32-ccid":
        stlink = any(F469_STLINK_SERIAL in p for p in by_id)
        # Serial-precise, not VID:PID: the F746 shares 046a:003e with a
        # different iSerial (Codex #188) — a VID:PID match would treat an
        # F746-plugged bench as a testable F469 and fail red, not skip.
        cherry = F469_ISERIAL in lsusb_serials("046a:003e")
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
        # Codex-#188 pattern (the nucula sibling, applied here 2026-10-26):
        # a board whose port exists but answers NEITHER a live pcscd reader
        # NOR the GemPC identity probe is in an unknown/foreign-firmware
        # state (bench case: the cashu-nfc-matrix T5 wallet — silent
        # console, no vpcd reader, no GemPC answer, reflash owner-gated)
        # — not evidence about the code under test. Trade-off accepted per
        # SKIP≠FAIL (#183): a wedged standard image ambers here too; the
        # place stays ours to re-verify interactively when that matters.
        # pcscd-listed GemPCTwin = standard image serving: testable, and
        # the port is pcscd-owned (never raw-probed under it).
        try:
            from smartcard.System import readers  # noqa: PLC0415

            serving = any("GemPCTwin" in str(r) for r in readers())
        except Exception:
            serving = False
        if not serving:
            from board_identity import probe_m5stick  # noqa: PLC0415

            probe = probe_m5stick()
            if not probe.get("ok"):
                return ("m5stick not answering the GemPC identity probe "
                        f"({str(probe.get('detail', '?'))[:40]}) — unknown/"
                        "foreign firmware variant (wallet/vpcd class); "
                        "GemPC legs skip until the standard image returns")
    elif place == "nucula-c3":
        # Codex #188 residual (found live 2026-10-15: the nightly failed
        # red instead of skipping when the board sat in a non-serving
        # state). SKIP≠FAIL doctrine: a nucula with NO serving reader
        # (broker OR wifi-IFD) and a CDC that does not answer the GemPC
        # identity probe is in an unknown/sibling-lane state — not
        # evidence about the code under test.
        from board_identity import probe_nucula  # noqa: PLC0415

        probe = probe_nucula()
        if not probe.get("ok"):
            return ("nucula CDC present but not serving (no broker reader, "
                    f"no raw GemPC answer: {probe.get('detail', '?')[:60]}) "
                    "— unknown/sibling-lane board state; legs skip per "
                    "SKIP≠FAIL (#183)")
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
