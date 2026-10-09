#!/usr/bin/env python3
"""Dynamic bench serial-port discovery.

Never hardcode ``/dev/ttyUSBx`` or ``/dev/ttyACMx``: USB re-enumeration
renumbers them (FTDI unbind/rebind, replug — see AGENTS.md "pcscd reader.conf
trap"). Resolve bench DUTs via their stable ``/dev/serial/by-id`` identities
instead — the same identities the labgrid exporter config pins
(tests/hardware/labgrid/exporter-ai-legion-nfc.yaml).

Bench doctrine rule 2 applies: zero or multiple matches is a LOUD error with
a listing, never a silent guess.

Env overrides: ``NUCULA_PORT`` / ``M5STICK_PORT`` win over detection, for
one-off rerouting without code edits.

Standalone usage:
    python3 tests/hardware/serial_ports.py            # resolve all
    python3 tests/hardware/serial_ports.py m5stick    # resolve one
"""

from __future__ import annotations

import glob
import os
import sys

# Stable USB identities per bench DUT (glob patterns against /dev/serial/by-id).
BENCH_PORT_GLOBS = {
    "nucula": "/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_*-if00",
    "m5stick": "/dev/serial/by-id/usb-Hades2001_M5stack_*-if00-port0",
}


def list_by_id() -> list[str]:
    """All currently present by-id serial devices (for error listings)."""
    return sorted(glob.glob("/dev/serial/by-id/*"))


def resolve(name: str) -> str:
    """Resolve a bench DUT's serial port by stable USB identity."""
    env = os.environ.get(f"{name.upper()}_PORT")
    if env:
        return env

    pattern = BENCH_PORT_GLOBS.get(name)
    if pattern is None:
        raise SystemExit(
            f"unknown bench port name {name!r}; known: {sorted(BENCH_PORT_GLOBS)}"
        )

    matches = sorted(p for p in glob.glob(pattern) if os.path.exists(p))
    if len(matches) == 1:
        return matches[0]

    listing = "\n".join(f"  {p}" for p in list_by_id()) or "  (none)"
    if not matches:
        raise SystemExit(
            f"no serial device matches {name!r} ({pattern}).\n"
            f"Present by-id devices:\n{listing}\n"
            f"Replug the device, or set {name.upper()}_PORT=/dev/... to override."
        )
    raise SystemExit(
        f"multiple serial devices match {name!r} ({pattern}):\n"
        + "\n".join(f"  {m}" for m in matches)
        + f"\nSet {name.upper()}_PORT=/dev/... to disambiguate."
    )


if __name__ == "__main__":
    for bench_name in sys.argv[1:] or list(BENCH_PORT_GLOBS):
        print(f"{bench_name}: {resolve(bench_name)}")
