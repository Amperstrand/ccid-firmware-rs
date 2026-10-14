#!/usr/bin/env python3
"""Unit tests for scripts/gen_status.py — stdlib only (the minimal CI
env must run them: no pytest, no pyscard). Run:

    python3 scripts/test_gen_status.py

Locks the STATUS.md contract (issue #183): every claim carries a
verification stamp, missing inputs degrade to explicit UNKNOWN rows,
and the generator NEVER fails the run that publishes status."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

GEN = Path(__file__).resolve().parent / "gen_status.py"


def run_gen(inventory: str, junit: str, *extra: str) -> tuple[int, str]:
    with tempfile.TemporaryDirectory() as d:
        inv = Path(d) / "inv.json"
        jx = Path(d) / "junit.xml"
        out = Path(d) / "STATUS.md"
        inv.write_text(inventory)
        jx.write_text(junit)
        r = subprocess.run(
            [sys.executable, str(GEN), "--inventory", str(inv),
             "--junit", str(jx), "--commit", "abcdef1234567890",
             "--date", "2026-10-14T18:00:00+00:00", "--out", str(out),
             *extra],
            capture_output=True, text=True,
        )
        body = out.read_text() if out.exists() else ""
        return r.returncode, body


GOOD_INV = json.dumps({
    "on_bench_host": True,
    "checks": [
        {"name": "coordinator reachable (51 resources total)", "ok": True, "detail": ""},
        {"name": "place stm32-ccid: acquire/release", "ok": False, "detail": "probe error"},
    ],
    "held_places": {"m5stick": "root@ai-legion"},
    "healthy": False,
})

GOOD_JUNIT = """<testsuites>
 <testsuite name="hil">
  <testcase classname="tests.hardware.labgrid.test_bench_state" name="test_a"/>
  <testcase classname="tests.hardware.labgrid.test_bench_state" name="test_b"><skipped/></testcase>
  <testcase classname="tests.hardware.labgrid.test_ccid_hil" name="test_c"><failure/></testcase>
 </testsuite>
</testsuites>"""


class GenStatusTests(unittest.TestCase):
    def test_exit_zero_and_stamp_on_healthy_inputs(self):
        rc, body = run_gen(GOOD_INV, GOOD_JUNIT)
        self.assertEqual(rc, 0)
        self.assertIn("abcdef123456", body)
        self.assertIn("2026-10-14T18:00:00+00:00", body)

    def test_inventory_rows_render_ok_and_fail_with_detail(self):
        _, body = run_gen(GOOD_INV, GOOD_JUNIT)
        self.assertIn("| coordinator reachable (51 resources total) | PASS |  |", body)
        self.assertIn("| place stm32-ccid: acquire/release | FAIL | probe error |", body)

    def test_held_places_listed_with_holder(self):
        _, body = run_gen(GOOD_INV, GOOD_JUNIT)
        self.assertIn("`m5stick` (root@ai-legion)", body)

    def test_junit_tallies_per_suite(self):
        _, body = run_gen(GOOD_INV, GOOD_JUNIT)
        self.assertIn(
            "| tests.hardware.labgrid.test_bench_state | 1 | 1 | 0 |", body)
        self.assertIn(
            "| tests.hardware.labgrid.test_ccid_hil | 0 | 0 | 1 |", body)

    def test_missing_junit_degrades_to_explicit_unknown_not_crash(self):
        rc, body = run_gen(GOOD_INV, "")
        self.assertEqual(rc, 0)
        self.assertIn("(HIL run unavailable)", body)

    def test_missing_inventory_degrades_to_explicit_unknown(self):
        rc, body = run_gen("", GOOD_JUNIT)
        self.assertEqual(rc, 0)
        self.assertIn("(inventory unavailable)", body)

    def test_generated_header_forbids_hand_edits(self):
        _, body = run_gen(GOOD_INV, GOOD_JUNIT)
        self.assertIn("GENERATED FILE", body)
        self.assertIn("never hand-edit", body.lower().replace("never", "never").replace("hand-edit", "hand-edit") or "hand-edit")


if __name__ == "__main__":
    unittest.main()
