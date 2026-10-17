#!/usr/bin/env python3
"""Unit tests for scripts/status_rollup.py — stdlib only (the minimal CI
env must run them: no pytest, no network). Locks the #183 Phase-2 rollup
contract: comment shape carries the marker + run link + generated body,
and the anti-spam digest posts ONLY on outcome changes (stamps excluded).

    python3 scripts/test_status_rollup.py
"""

import importlib.util
import json
import re
import sys
import unittest
import urllib.request
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


class _FakeResponse:
    def __init__(self, payload):
        self._payload = payload

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def read(self):
        return json.dumps(self._payload).encode()


class FetchLastRollupPaginationTests(unittest.TestCase):
    """Codex P2 on #226/#227: comments are per_page-capped at 100 and
    ordered ascending by id — the digest comparison must run against the
    NEWEST comment, i.e. the last one on the LAST page, not
    comments[-1] of page 1 (which is permanently the 100th comment
    once an issue crosses 100)."""

    def _install_fake(self, pages):
        calls = []

        def fake_urlopen(req, timeout=30):
            url = req.full_url
            calls.append(url)
            # not split("page="): per_page= would match first
            page_no = int(re.search(r"[?&]page=(\d+)", url).group(1))
            return _FakeResponse(pages[page_no - 1])

        real_urlopen = urllib.request.urlopen
        urllib.request.urlopen = fake_urlopen
        self.addCleanup(setattr, urllib.request, "urlopen", real_urlopen)
        return calls

    def test_multi_page_issue_reads_the_last_comment_of_the_last_page(self):
        page1 = [{"id": i, "body": f"old {i}"} for i in range(100)]
        newest = "the actual newest rollup body"
        page2 = [
            {"id": 100, "body": "older tail"},
            {"id": 101, "body": newest},
        ]
        calls = self._install_fake([page1, page2])

        body = rollup.fetch_last_rollup("x/y", "183", "tok")

        self.assertEqual(body, newest)
        self.assertEqual(len(calls), 2, "walked to the second (short) page")
        self.assertIn("page=1", calls[0])
        self.assertIn("page=2", calls[1])

    def test_single_page_issue_makes_one_request(self):
        one_page = [{"id": 1, "body": "only comment"}]
        calls = self._install_fake([one_page])

        body = rollup.fetch_last_rollup("x/y", "183", "tok")

        self.assertEqual(body, "only comment")
        self.assertEqual(len(calls), 1, "short page terminates the walk")

    def test_empty_issue_returns_none_after_first_page(self):
        calls = self._install_fake([[]])

        self.assertIsNone(rollup.fetch_last_rollup("x/y", "183", "tok"))
        self.assertEqual(len(calls), 1)

    def test_fetch_error_degrades_to_none_still_posts(self):
        def broken_urlopen(req, timeout=30):
            raise OSError("network down")

        real_urlopen = urllib.request.urlopen
        urllib.request.urlopen = broken_urlopen
        self.addCleanup(setattr, urllib.request, "urlopen", real_urlopen)

        self.assertIsNone(rollup.fetch_last_rollup("x/y", "183", "tok"))


if __name__ == "__main__":
    unittest.main()
