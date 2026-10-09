#!/usr/bin/env python3
"""Bench inventory check: what is connected, exported, and working.

One command answers: are all labgrid resources up, do the places acquire,
does pcscd see every reader, and is the DUT/reference identity split clean?

    python3 tests/hardware/labgrid/bench_inventory.py

Exit code 0 = bench fully healthy; 1 = at least one check failed.
"""

import subprocess
import sys

DUT_SERIAL = "ST2XXX-001"  # F469 Cherry-emulation iSerial
EXPECTED_EXPORTS = {
    "ai-legion-nfc/nucula-c3/SerialPort",
    "ai-legion-nfc/m5stick-ftdi/SerialPort",
    "ai-legion-nfc/m5stick-usb/SerialPort",
    "ai-legion-nfc/stm32f469-stlink/SerialPort",
    "ai-legion-nfc/stm32f469-ccid/USBDevice",
    "ai-legion-nfc/ref-acr1252/USBDevice",
    "ai-legion-nfc/ref-cardman/USBDevice",
}
EXPECTED_PLACES = ["stm32-ccid", "nucula-c3", "m5stick", "ref-acr1252", "ref-cardman"]
# readers pcscd must list, keyed by stable identity fragment
EXPECTED_PCSCD = ["ST2XXX-001", "ACR1252", "CardMan"]

failures = []


def sh(cmd: str) -> str:
    r = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=30)
    return r.stdout


def check(name: str, ok: bool, detail: str = ""):
    mark = "OK  " if ok else "FAIL"
    print(f"  [{mark}] {name}" + (f" — {detail}" if detail and not ok else ""))
    if not ok:
        failures.append(name)


print("== labgrid coordinator ==")
resources = [l.strip() for l in sh("labgrid-client resources").splitlines() if l.strip()]
check(f"coordinator reachable ({len(resources)} resources total)", bool(resources))
exported = {r for r in resources if r.startswith("ai-legion-nfc/")}
missing = EXPECTED_EXPORTS - exported
check(f"all 7 bench resources exported", not missing, f"missing: {sorted(missing)}")

print("== labgrid places ==")
for place in EXPECTED_PLACES:
    r = subprocess.run(["labgrid-client", "-p", place, "acquire"],
                       capture_output=True, text=True, timeout=15)
    if r.returncode == 0:
        subprocess.run(["labgrid-client", "-p", place, "release"],
                       capture_output=True, timeout=15)
        check(f"place {place}: acquire/release", True)
    else:
        held = "already acquired" in (r.stderr or "") or "different user" in (r.stderr or "")
        check(f"place {place}: acquire/release", held,
              f"in use by another session ({r.stderr.strip()[:80]})" if held else r.stderr.strip()[:100])

print("== pcscd readers ==")
readers_out = sh(
    "python3 -c 'from smartcard.System import readers; "
    "[print(str(r)) for r in readers()]'"
)
readers = [l for l in readers_out.splitlines() if l.strip()]
check(f"pcscd lists readers ({len(readers)})", bool(readers), readers_out[:120])
for frag in EXPECTED_PCSCD:
    check(f"reader identity '{frag}' present",
          any(frag in r for r in readers),
          "; ".join(readers))

print("== DUT/reference identity split ==")
dut = [r for r in readers if DUT_SERIAL in r]
check(f"exactly one DUT reader (serial {DUT_SERIAL})", len(dut) == 1, "; ".join(dut))
refs = [r for r in readers if DUT_SERIAL not in r and ("ACR1252" in r or "CardMan" in r)]
check(f"reference readers carry no DUT serial ({len(refs)} found)", len(refs) >= 2, "; ".join(refs))

print("== USB device presence ==")
lsusb = sh("lsusb")
for vidpid, label in [("046a:003e", "F469 DUT (Cherry emul)"),
                      ("0483:374b", "ST-LINK V2.1"),
                      ("072f:223b", "ACR1252 reference"),
                      ("076b:3021", "CardMan reference")]:
    check(f"{label} on USB", vidpid in lsusb)

print()
if failures:
    print(f"INVENTORY: {len(failures)} FAILED — {', '.join(failures)}")
    sys.exit(1)
print("INVENTORY: all checks passed — bench fully healthy")
