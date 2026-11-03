#!/usr/bin/env python3
"""Unit tests for scripts/spec_alignment.py (stdlib only, CI-runnable).

The end-to-end stages (greatspectate check/coverage) are integration-
tested on demand via `python3 scripts/spec_alignment.py` on a checkout
with submodules — these unit tests cover the pure logic: per-source
statistics, gap-ignore separation, and markdown rendering.
"""
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "spec_alignment.py"
spec = importlib.util.spec_from_file_location("spec_alignment", SCRIPT)
sa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sa)


def doc(source="TEST", lines=None):
    return {
        "source": source,
        "id": None,
        "path": f"reference/{source}.txt",
        "lines": [
            {"line": n + 1, "text": t, "status": st, "mentions": m}
            for n, (t, st, m) in enumerate(lines or [])
        ],
    }


class DocStatsTests(unittest.TestCase):
    def test_counts_and_pct(self):
        stats = sa.doc_stats(doc(lines=[
            ("#include <x.h>", "neutral", 0),
            ("MUST set foo to 1", "covered", 2),
            ("MUST NOT send bar", "gap", 0),
            ("SHOULD baz", "covered", 1),
            (" * comment", "neutral", 0),
        ]))
        self.assertEqual(stats["covered"], 2)
        self.assertEqual(stats["gap"], 1)
        self.assertEqual(stats["neutral"], 2)
        self.assertEqual(stats["quotable"], 3)
        self.assertEqual(stats["pct"], 66.7)
        self.assertEqual(stats["gap_lines"], [(3, "MUST NOT send bar")])

    def test_empty_source_has_null_pct(self):
        stats = sa.doc_stats(doc(lines=[("#include", "neutral", 0)]))
        self.assertIsNone(stats["pct"])
        self.assertEqual(stats["quotable"], 0)

    def test_gap_ignore_is_separated_not_lost(self):
        # reader.h line 24 is in GAP_IGNORES (BSD disclaimer prose)
        lines = [(f"filler {i}", "neutral", 0) for i in range(23)]
        stats = sa.doc_stats(doc(source="PCSC", lines=lines + [
            ("IN NO EVENT SHALL THE AUTHOR BE LIABLE", "gap", 0),  # line 24
            ("MUST do y", "gap", 0),                     # line 25
            ("MUST do x", "covered", 1),                 # line 26
        ]))
        self.assertEqual(stats["covered"], 1)
        self.assertEqual(stats["gap"], 1)
        self.assertEqual(stats["ignored_gaps"], [24])
        self.assertEqual(stats["gap_lines"], [(25, "MUST do y")])


class RenderTests(unittest.TestCase):
    def setUp(self):
        self.meta = {
            "generated": 1700000000.0,
            "git_rev": "deadbeef",
            "tool_version": "0.2.3-test",
            "config_sha8": "ab12cd34",
        }
        self.stats = [
            {"source": "B", "path": "ref/b.md", "covered": 4, "gap": 0,
             "gap_lines": [], "ignored_gaps": [], "neutral": 10,
             "quotable": 4, "pct": 100.0},
            {"source": "A", "path": "ref/a.md", "covered": 1, "gap": 1,
             "gap_lines": [(7, "MUST respond with 6F00")],
             "ignored_gaps": [], "neutral": 3, "quotable": 2, "pct": 50.0},
        ]

    def test_table_rows_and_totals(self):
        md = sa.render_markdown(self.meta, self.stats, drift=[])
        self.assertIn("| A | 1 | 1 | 2 | 50.0% |", md)
        self.assertIn("| B | 4 | 0 | 4 | 100.0% |", md)
        self.assertIn("PASS (every quote matches its source)", md)
        self.assertIn("`deadbeef`", md)
        self.assertIn("`ab12cd34`", md)  # config provenance
        # A before B: deterministic sort by source name
        self.assertLess(md.index("| A |"), md.index("| B |"))

    def test_drift_section_on_fail(self):
        md = sa.render_markdown(self.meta, self.stats,
                                drift=["src/x.rs:42:cannot find match"])
        self.assertIn("FAIL", md)
        self.assertIn("src/x.rs:42:cannot find match", md)

    def test_gap_excerpts_listed_with_path_line(self):
        md = sa.render_markdown(self.meta, self.stats, drift=[])
        self.assertIn("ref/a.md:7", md)
        self.assertIn("MUST respond with 6F00", md)

    def test_ignored_gaps_section(self):
        self.stats[1]["ignored_gaps"] = [24]
        md = sa.render_markdown(self.meta, self.stats, drift=[])
        self.assertIn("Ignored gaps", md)
        self.assertIn("A line 24", md)


class EndToEndTests(unittest.TestCase):
    """Full run — only on a checkout with greatspectate + submodules."""

    def test_full_run_against_repo(self):
        import shutil
        if not shutil.which("greatspectate"):
            self.skipTest("greatspectate not installed")
        # A real submodule FILE (the dir exists but is empty before
        # `git submodule update --init` — that must skip, not fail).
        if not (sa.REPO / "reference/osmo-ccid-firmware/ccid_common"
                / "ccid_proto.h").exists():
            self.skipTest("submodules not checked out")
        with tempfile.TemporaryDirectory() as tmp:
            rc = sa.main(["--out", tmp])
            self.assertIn(rc, (0, 1))  # 1 = drift found (still valid run)
            summary = json.loads(
                (Path(tmp) / "summary.json").read_text())
            self.assertIn("documents", summary)
            self.assertTrue(summary["documents"])
            md = (Path(tmp) / "SPEC-ALIGNMENT.md").read_text()
            self.assertIn("Spec alignment", md)
            sources = {d["source"] for d in summary["documents"]}
            self.assertIn("CCID_SPEC", sources)


if __name__ == "__main__":
    unittest.main()
