#!/usr/bin/env python3
"""Generate docs/STATUS.md from machine evidence (issue #183).

Consumes the artifacts the hil-bench nightly workflow produces:
  - bench_inventory.py --json output  (what's wired/exported/acquirable)
  - pytest JUnit XML from the HIL run (what passed/skipped per suite)

Emits a single STATUS.md whose every claim carries a verification stamp
(commit + date + source) — the anti-#110 doctrine applied to status:
no hand-written works/doesn't-work statements, generated only.

    python3 scripts/gen_status.py \
        --inventory inventory.json --junit hil-junit.xml \
        --commit <sha> --date <iso8601> [--out docs/STATUS.md]

Exit 0 always (a status generator must not fail the workflow that
publishes status); malformed inputs degrade to explicit UNKNOWN rows.
"""

import argparse
import json
import sys
import xml.etree.ElementTree as ET
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

HEADER = """<!-- GENERATED FILE — do not edit by hand (issue #183).
     Source of truth: scripts/gen_status.py fed by the hil-bench nightly
     workflow (bench inventory JSON + HIL JUnit XML). Every claim below
     carries its verification stamp. Regenerate; never hand-edit. -->

# Hardware & Firmware Status

> Auto-generated from the bench nightly run. `PASS` = verified by test;
> `SKIP` = hardware absent/busy (never a code failure); `FAIL` = broken.
> Human-readable history: the workflow run linked from the stamp.

"""


def load_inventory(path: Path) -> dict:
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as e:
        print(f"gen_status: inventory unreadable ({e}) — degrading", file=sys.stderr)
        return {"checks": [], "held_places": {}, "healthy": None, "on_bench_host": None}


def load_junit(path: Path) -> dict:
    """testcase classname/name -> outcome, plus per-suite tallies."""
    outcomes = {}
    suites = defaultdict(lambda: {"passed": 0, "skipped": 0, "failed": 0})
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError) as e:
        print(f"gen_status: junit unreadable ({e}) — degrading", file=sys.stderr)
        return {"cases": outcomes, "suites": suites}
    for case in root.iter("testcase"):
        name = f"{case.get('classname', '')}::{case.get('name', '')}"
        if case.find("failure") is not None or case.find("error") is not None:
            outcome = "FAIL"
        elif case.find("skipped") is not None:
            outcome = "SKIP"
        else:
            outcome = "PASS"
        outcomes[name] = outcome
        tally_key = {"PASS": "passed", "SKIP": "skipped", "FAIL": "failed"}[outcome]
        suites[case.get("classname", "(root)")][tally_key] += 1
    return {"cases": outcomes, "suites": suites}


def bench_section(inv: dict, stamp: str) -> str:
    lines = ["## Bench wiring (labgrid)\n"]
    checks = inv.get("checks", [])
    if not checks:
        lines.append("| check | state | detail |\n|---|---|---|\n"
                     "| (inventory unavailable) | UNKNOWN | regenerate nightly |\n")
    else:
        lines.append("| check | state | detail |\n|---|---|---|\n")
        for c in checks:
            state = "PASS" if c["ok"] else "FAIL"
            lines.append(f"| {c['name']} | {state} | {c.get('detail', '') or ''} |\n")
    held = inv.get("held_places", {})
    if held:
        lines.append("\nHeld (healthy-but-busy, probes skipped): "
                     + ", ".join(f"`{p}` ({h})" for p, h in sorted(held.items()))
                     + "\n")
    lines.append(f"\nVerified: {stamp} — `bench_inventory.py`\n")
    return "".join(lines)


def hil_section(junit: dict, stamp: str) -> str:
    lines = ["## HIL suites (on-device)\n"]
    suites = junit.get("suites", {})
    if not suites:
        lines.append("| suite | pass | skip | fail |\n|---|---|---|---|\n"
                     "| (HIL run unavailable) | – | – | – |\n")
    else:
        lines.append("| suite | pass | skip | fail |\n|---|---|---|---|\n")
        for cls in sorted(suites):
            t = suites[cls]
            lines.append(f"| {cls} | {t['passed']} | {t['skipped']} | {t['failed']} |\n")
    lines.append(f"\nVerified: {stamp} — nightly HIL selection per "
                 "`.github/workflows/hil-bench.yml` "
                 "(SKIP = place busy/hardware absent, per doctrine)\n")
    return "".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--inventory", required=True, type=Path)
    ap.add_argument("--junit", required=True, type=Path)
    ap.add_argument("--commit", required=True)
    ap.add_argument("--date", default=datetime.now(timezone.utc).isoformat(timespec="seconds"))
    ap.add_argument("--out", type=Path, default=Path("docs/STATUS.md"))
    args = ap.parse_args()

    stamp = f"{args.date} @ `{args.commit[:12]}`"
    inv = load_inventory(args.inventory)
    junit = load_junit(args.junit)

    body = (HEADER + bench_section(inv, stamp) + "\n" + hil_section(junit, stamp)
            + "\n## Board-level notes\n\n"
            + "Per-board deep state (BLE, GP cards, known gaps) lives in the "
            "tracking issues it belongs to (#66 BLE, #127 GP cards, #183 this "
            "pipeline). This file only carries what a run just verified.\n")

    args.out.write_text(body)
    print(f"gen_status: wrote {args.out} ({len(inv.get('checks', []))} inventory checks, "
          f"{sum(len(v) for v in junit.get('suites', {}).values()) if junit.get('suites') else 0} test outcomes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
