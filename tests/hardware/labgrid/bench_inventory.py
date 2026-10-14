#!/usr/bin/env python3
"""Bench inventory check: what is connected, exported, and working.

One command answers: are all labgrid resources up, do the places acquire,
does pcscd see every reader, and is the DUT/reference identity split clean?

    python3 tests/hardware/labgrid/bench_inventory.py
    python3 tests/hardware/labgrid/bench_inventory.py --json   # machine mode

Machine mode (issue #183): prints a single JSON object to stdout
(structure below) and exits with the same 0/1 semantics — consumed by
the hil-bench nightly workflow and scripts/gen_status.py to build
docs/STATUS.md. Human output is suppressed in --json mode.

    {"host": {"on_bench_host": bool},
     "checks": [{"name": str, "ok": bool, "detail": str}, ...],
     "held_places": {"place": "holder"},
     "healthy": bool}

Expected exports are PARSED from the in-repo exporter configs
(tests/hardware/labgrid/exporter-ai-legion-nfc.yaml +
tools/hil/labgrid-exporter.yaml) — the configs are the single source of
truth, so this check cannot drift from the wiring it validates (the
2026-10-14 audit found a hardcoded list two bench rewirings stale).

The pcscd/USB/identity sections are BENCH-HOST checks: they probe
locally-attached hardware. Run them on the bench host (.208); anywhere
else the script detects the absence of ALL bench USB ids and skips those
sections with a banner instead of printing a wall of misleading FAILs.

Exit code 0 = bench healthy (or healthy-where-applicable); 1 = failure.
"""

import argparse
import io
import json
import re
import shutil
import subprocess
import sys
from contextlib import redirect_stdout
from pathlib import Path

DUT_SERIAL = "ST2XXX-001"  # F469 Cherry-emulation iSerial
HERE = Path(__file__).resolve().parent
EXPORTER_CONFIGS = {
    "ai-legion-nfc": HERE / "exporter-ai-legion-nfc.yaml",
    "ai-legion-ccid": HERE.parent.parent.parent / "tools" / "hil" / "labgrid-exporter.yaml",
}
EXPECTED_PLACES = [
    "stm32-ccid", "nucula-c3", "m5stick", "ref-acr1252", "ref-cardman",
    "stm32-f746", "ccid-nucula", "ccid-m5stick",
]
# readers pcscd must list, keyed by stable identity fragment
EXPECTED_PCSCD = ["ST2XXX-001", "ACR1252", "CardMan"]
# (vid:pid, label) pairs that identify the bench host's USB tree; if NONE
# are present, the pcscd/USB sections are skipped as not-on-bench-host.
BENCH_USB_IDS = [
    ("046a:003e", "F469/F746 DUT (Cherry emul)"),
    ("0483:374b", "ST-LINK V2.1"),
    ("072f:223b", "ACR1252 reference"),
    ("076b:3021", "CardMan reference"),
]

failures = []
checks = []
held = {}


def sh(cmd: str) -> str:
    r = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=30)
    return r.stdout


def held_places() -> dict:
    """place -> holder, from `labgrid-client who` (issue #110): a place
    another session acquired is healthy-but-busy — probe attempts against
    it are noise (and acquire/release proves nothing while held)."""
    out = sh("labgrid-client who")
    for line in out.splitlines()[1:]:  # skip "User Host Place Changed" header
        parts = line.split()
        if len(parts) >= 4:
            held[parts[2]] = f"{parts[0]}@{parts[1]}"
    return held


def check(name: str, ok: bool, detail: str = ""):
    mark = "OK  " if ok else "FAIL"
    print(f"  [{mark}] {name}" + (f" — {detail}" if detail and not ok else ""))
    checks.append({"name": name, "ok": bool(ok), "detail": detail if not ok else ""})
    if not ok:
        failures.append(name)


def expected_groups() -> set:
    """Parse (exporter, group) pairs from the in-repo configs.

    Group-level on purpose: the coordinator may serve a declared class
    under a proxied name (the OTA seats declare USBSerialPort, the
    coordinator lists NetworkSerialPort for the same group), so exact
    class-name prediction is fragile — group presence is the invariant
    that matters ("is the wiring exported?").
    """
    expected = set()
    for exporter, path in EXPORTER_CONFIGS.items():
        if not path.is_file():
            check(f"exporter config {path.name} present", False, "file missing")
            continue
        for line in path.read_text().splitlines():
            m = re.match(r"^([A-Za-z0-9_-]+):\s*$", line)
            if m:
                expected.add((exporter, m.group(1)))
    return expected


def run_body():
    print("== labgrid coordinator ==")
    resources = [l.strip() for l in sh("labgrid-client resources").splitlines() if l.strip()]
    check(f"coordinator reachable ({len(resources)} resources total)", bool(resources))
    exported_groups = {
        tuple(r.split("/")[:2]) for r in resources
        if r.startswith(("ai-legion-nfc/", "ai-legion-ccid/"))
    }
    want = expected_groups()
    missing = want - exported_groups
    check(f"all {len(want)} configured resource groups exported", not missing,
          f"missing: {sorted('/'.join(g) for g in missing)}")

    print("== labgrid places ==")
    held_places()
    for place in EXPECTED_PLACES:
        if place in held:
            # Held = healthy-but-busy: counted as passing, never probed.
            print(f"  [HELD] place {place}: acquired by {held[place]} — acquire/release probe skipped")
            checks.append({"name": f"place {place}: acquire/release",
                           "ok": True, "detail": f"held by {held[place]} (probe skipped)"})
            continue
        r = None
        try:
            r = subprocess.run(["labgrid-client", "-p", place, "acquire"],
                               capture_output=True, text=True, timeout=15)
        except (OSError, subprocess.TimeoutExpired) as e:
            check(f"place {place}: acquire/release", False, f"probe error: {e}")
            continue
        if r.returncode == 0:
            try:
                subprocess.run(["labgrid-client", "-p", place, "release"],
                               capture_output=True, timeout=15)
            except (OSError, subprocess.TimeoutExpired):
                pass
            check(f"place {place}: acquire/release", True)
        else:
            check(f"place {place}: acquire/release", False, r.stderr.strip()[:100])

    lsusb = sh("lsusb")
    on_bench_host = any(vidpid in lsusb for vidpid, _ in BENCH_USB_IDS)
    if not on_bench_host:
        print("== pcscd / USB: SKIPPED — not the bench host ==")
        print("  (none of the bench USB ids are locally attached; these sections")
        print("   check locally-plugged readers and belong on the bench host, .208)")
    else:
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
        for vidpid, label in BENCH_USB_IDS:
            check(f"{label} on USB", vidpid in lsusb)

    print("== disk pressure (issue #108) ==")
    # The 2026-10-10 incident: /root/.cargo-target grew until the disk filled
    # and HIL runs died. 85% is the warning line, 90% fails the inventory.
    seen_fs = set()
    for p in ("/root/.cargo-target", "/tmp/opencode"):
        try:
            usage = shutil.disk_usage(p)
        except OSError:
            continue
        pct = usage.used * 100 // usage.total
        fs_key = (usage.total, usage.free)
        if fs_key in seen_fs:
            continue
        seen_fs.add(fs_key)
        detail = f"{pct}% used ({usage.used // (1 << 30)}G/{usage.total // (1 << 30)}G)"
        if pct >= 85:
            check(f"disk under {p} below 85%", pct < 90, detail)
        else:
            check(f"disk under {p} below 85%", True, detail)

    print()
    return on_bench_host


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--json", action="store_true",
                        help="machine mode: single JSON object on stdout, human output suppressed")
    args = parser.parse_args()

    if args.json:
        buf = io.StringIO()
        with redirect_stdout(buf):
            on_bench_host = run_body()
        print(json.dumps({
            "on_bench_host": on_bench_host,
            "checks": checks,
            "held_places": held,
            "healthy": not failures,
        }))
    else:
        on_bench_host = run_body()
        if failures:
            print(f"INVENTORY: {len(failures)} FAILED — {', '.join(failures)}")
            return 1
        print("INVENTORY: all checks passed — bench fully healthy"
              + ("" if on_bench_host else " (bench-host sections skipped on this host)"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
