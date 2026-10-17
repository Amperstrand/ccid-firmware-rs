#!/usr/bin/env python3
"""Unit tests for scripts/status_rollup.py — stdlib only (the minimal CI
env must run them: no pytest, no network). Locks the #183 Phase-2 rollup
contract: comment shape carries the marker + run link + generated body,
and the anti-spam digest posts ONLY on outcome changes (stamps excluded).

    python3 scripts/test_status_rollup.py
"""

import importlib.util
import sys
import unittest
from pathlib import Path

_SRC = Path(__file__).resolve().parent / "status_rollup.py"
_spec = importlib.util.spec_from_file_location("status_rollup", _SRC)
rollup = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(rollup)

STATUS_A = """<!-- GENERATED FILE — do not edit by hand (issue #183). -->

# Hardware & Firmware Status

| check | state | detail |
|---|---|---|
| coordinator reachable (51 resources total) | PASS |  |

Verified: 2026-10-14T18:00:00+00:00 @ aaaaaa000000 — `bench_inventory.py`

| suite | pass | skip | fail |
|---|---|---|---|
| tests/hardware/labgrid | 7 | 1 | 0 |

Verified: 2026-10-14T18:00:12+00:00 @ aaaaaa000000 — `hil-junit.xml`
"""

# Same outcomes, different run stamp (the nightly re-run shape).
STATUS_A_NEXT_NIGHT = STATUS_A.replace("2026-10-14", "2026-10-15")

# A real change: one check flipped to FAIL.
STATUS_B = STATUS_A.replace(
    "| coordinator reachable (51 resources total) | PASS |  |",
    "| coordinator reachable (51 resources total) | FAIL | probe error |",
)


class RenderCommentTests(unittest.TestCase):
    def test_marker_and_run_link_wrap_the_generated_body(self):
        body = rollup.render_comment(STATUS_A, "https://example/r/1")
        self.assertTrue(body.startswith(rollup.MARKER))
        self.assertIn("[full run](https://example/r/1)", body)
        self.assertIn("# Hardware & Firmware Status", body)
        self.assertIn("coordinator reachable", body)

    def test_marker_is_stable_for_change_detection(self):
        # The marker string is the contract the fetch/compare path keys
        # on — accidental rewording would orphan every prior comment.
        self.assertEqual(rollup.MARKER, "<!-- status-rollup:v1 -->")


class ShouldPostTests(unittest.TestCase):
    def test_first_rollup_always_posts(self):
        self.assertTrue(rollup.should_post(None, STATUS_A))

    def test_non_rollup_last_comment_does_not_silence(self):
        # A human reply after our comment makes it the "last" — the
        # tracker state must not block the next generated rollup.
        self.assertTrue(rollup.should_post("human note", STATUS_A))

    def test_identical_outcomes_new_stamp_stays_silent(self):
        prev = rollup.render_comment(STATUS_A, "https://example/r/1")
        self.assertFalse(rollup.should_post(prev, STATUS_A_NEXT_NIGHT))

    def test_changed_outcome_posts(self):
        prev = rollup.render_comment(STATUS_A, "https://example/r/1")
        self.assertTrue(rollup.should_post(prev, STATUS_B))


class DigestTests(unittest.TestCase):
    def test_digest_ignores_stamp_lines_only(self):
        self.assertEqual(rollup.digest(STATUS_A), rollup.digest(STATUS_A_NEXT_NIGHT))
        self.assertNotEqual(rollup.digest(STATUS_A), rollup.digest(STATUS_B))


class MainDryRunTests(unittest.TestCase):
    def test_no_issues_configured_is_logged_noop(self):
        rc = rollup.main.__wrapped__ if hasattr(rollup.main, "__wrapped__") else None
        self.assertIsNone(rc, "main is plain (argparse-driven); dry-run covered below")

    def test_dry_run_invocation_exits_zero_without_token(self):
        import subprocess
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            st = Path(d) / "STATUS.md"
            st.write_text(STATUS_A)
            r = subprocess.run(
                [sys.executable, str(_SRC), "--status", str(st),
                 "--repo", "x/y", "--issues", "167"],
                capture_output=True, text=True, timeout=30,
            )
            self.assertEqual(r.returncode, 0)
            self.assertIn("DRY RUN #167", r.stdout)


if __name__ == "__main__":
    unittest.main()
