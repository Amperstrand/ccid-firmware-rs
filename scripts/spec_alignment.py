#!/usr/bin/env python3
"""spec_alignment.py — greatspectations wrapper: drift gate + coverage self-audit.

Owner directive 2026-11-03: use Rusty Russell's greatspectations
(github.com/rustyrussell/greatspectations, MIT) for spec-code alignment
and keep the results organized so coverage and alignment can be
self-audited and visualized.

One command runs the whole lifecycle documented in specquotes.toml:

  1. CHECK (the gate): every spec quote embedded in the Rust tree must
     still match its reference source. Drift fails the run with
     file:line messages (exit 1).
  2. COVERAGE (the self-audit): which requirement lines of each source
     are quoted (covered) vs. implementable-but-unquoted (gap).
  3. ARTIFACTS (the organization): summary.json (machine audit; carries
     git rev + specquotes.toml sha256 so a saved artifact is traceable
     to one tree state) and SPEC-ALIGNMENT.md (the human/visual table),
     both under --out (default .omo/spec-alignment, gitignored — upload
     as CI artifacts, do not commit; the HTML renders reference-source
     text under submodule licenses). --html additionally writes one
     visualization page per document.

Usage:
  python3 scripts/spec_alignment.py [--out DIR] [--html] [--check-only]
                                    [--files GLOB ...]

Exit: 0 no drift; 1 quote drift (or unignored gaps with --check-gaps);
2 environment error (missing tool/submodules).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
CONFIG = "specquotes.toml"
DEFAULT_GLOBS = ("crates/*/src/*.rs", "firmware/*/src/*.rs")

# Lines reported as coverage "gap" that are NOT requirements — license
# boilerplate prose inside a reference source. Keep this list short and
# commented; anything else showing as a gap is a real self-audit signal
# (an implementable clause no code quotes).
GAP_IGNORES = {
    # reader.h: BSD disclaimer continuation — prose, not a requirement.
    ("PCSC", 24),
}


def run(cmd, cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


def rust_files(repo: Path, globs) -> list:
    proc = run(["git", "ls-files", *globs], repo)
    if proc.returncode != 0:
        raise RuntimeError(f"git ls-files failed: {proc.stderr.strip()}")
    return [f for f in proc.stdout.splitlines() if f]


def tool_version() -> str:
    proc = run(["greatspectate", "--version"], REPO)
    return proc.stdout.strip() or proc.stderr.strip() or "unknown"


def git_rev(repo: Path) -> str:
    proc = run(["git", "rev-parse", "HEAD"], repo)
    return proc.stdout.strip() if proc.returncode == 0 else "unknown"


def run_check(repo: Path, out_dir: Path, files: list, cov_path: Path):
    """The drift gate. Returns (rc, messages)."""
    proc = run(
        ["greatspectate", "check", "--config", CONFIG,
         "--comment-start", "// ", "--comment-continue", "//",
         "-k", "--coverage", str(cov_path), *files],
        repo)
    messages = [ln for ln in proc.stdout.splitlines()
                if ln.strip() and not ln.startswith(("Matched", "matched"))]
    return proc.returncode, messages


def run_coverage(repo: Path, cov_path: Path) -> dict:
    # Exit-code semantics (greatspectations cli.py cmd_coverage):
    # 0 = clean, 1 = uncovered (gap) lines exist — a VALID result.
    # Only >=2 / "error:" output is a real failure.
    proc = run(
        ["greatspectate", "coverage", "--config", CONFIG,
         "--coverage", str(cov_path), "--format", "json"], repo)
    if proc.returncode not in (0, 1) or not proc.stdout.strip():
        raise RuntimeError(
            f"greatspectate coverage failed "
            f"(stderr: {proc.stderr.strip()[:200]}; "
            f"stdout: {proc.stdout.strip()[:200]})")
    return json.loads(proc.stdout)


def doc_stats(doc: dict) -> dict:
    """Per-source coverage numbers; ignored gaps are separated, not lost."""
    covered = gap = neutral = 0
    gap_lines, ignored_lines = [], []
    for line in doc["lines"]:
        status = line["status"]
        if status == "covered":
            covered += 1
        elif status == "gap":
            if (doc["source"], line["line"]) in GAP_IGNORES:
                ignored_lines.append(line["line"])
            else:
                gap += 1
                gap_lines.append((line["line"], line["text"]))
        else:
            neutral += 1
    quotable = covered + gap
    try:  # repo-relative paths keep artifacts portable across checkouts
        path = str(Path(doc["path"]).resolve().relative_to(REPO.resolve()))
    except ValueError:
        path = doc["path"]
    return {
        "source": doc["source"],
        "path": path,
        "covered": covered,
        "gap": gap,
        "gap_lines": gap_lines,
        "ignored_gaps": ignored_lines,
        "neutral": neutral,
        "quotable": quotable,
        "pct": round(100.0 * covered / quotable, 1) if quotable else None,
    }


def render_markdown(meta: dict, stats: list, drift: list) -> str:
    lines = [
        "# Spec alignment — greatspectations report",
        "",
        f"- generated: {meta['generated']} ({time.strftime('%Y-%m-%d %H:%M:%S %z', time.localtime(meta['generated']))})",
        f"- git rev: `{meta['git_rev']}`",
        f"- tool: greatspectations {meta['tool_version']}",
        f"- config: specquotes.toml sha256 `{meta['config_sha8']}`",
        f"- drift gate: {'PASS (every quote matches its source)' if not drift else 'FAIL'}",
        "",
        "Coverage = quoted / quotable requirement lines per reference"
        " source (neutral boilerplate excluded). A real gap is an",
        "implementable clause no code quotes — either unimplemented"
        " behavior or missing provenance.",
        "",
        "| source | covered | gap | quotable | coverage | path |",
        "|---|---:|---:|---:|---:|---|",
    ]
    for s in sorted(stats, key=lambda s: s["source"]):
        pct = f"{s['pct']}%" if s["pct"] is not None else "—"
        lines.append(
            f"| {s['source']} | {s['covered']} | {s['gap']} | "
            f"{s['quotable']} | {pct} | `{s['path']}` |")
    totals = {
        "covered": sum(s["covered"] for s in stats),
        "gap": sum(s["gap"] for s in stats),
        "quotable": sum(s["quotable"] for s in stats),
    }
    lines += [
        "",
        f"**Totals: {totals['covered']} covered / {totals['quotable']} "
        f"quotable ({round(100.0 * totals['covered'] / totals['quotable'], 1) if totals['quotable'] else 0}%), "
        f"{totals['gap']} unignored gaps**",
        "",
    ]
    if drift:
        lines += ["## Quote drift (gate FAIL)", ""] + \
            [f"- {m}" for m in drift] + [""]
    gaps = [s for s in stats if s["gap_lines"]]
    if gaps:
        lines += ["## Unquoted requirement lines (gaps)", ""]
        for s in gaps:
            lines.append(f"### {s['source']} (`{s['path']}`)")
            for lineno, text in s["gap_lines"][:20]:
                lines.append(f"- `{s['path']}:{lineno}`: {text.strip()[:90]}")
            if len(s["gap_lines"]) > 20:
                lines.append(f"- … {len(s['gap_lines']) - 20} more")
            lines.append("")
    ignored = [s for s in stats if s["ignored_gaps"]]
    if ignored:
        lines += ["## Ignored gaps (license boilerplate, not requirements)", ""]
        for s in ignored:
            for lineno in s["ignored_gaps"]:
                why = "BSD disclaimer continuation" \
                    if (s["source"], lineno) in GAP_IGNORES else ""
                lines.append(f"- {s['source']} line {lineno}: {why}")
        lines.append("")
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        description=("spec alignment drift gate + coverage self-audit "
                     "(greatspectations wrapper)"))
    ap.add_argument("--out", default=".omo/spec-alignment",
                    help="artifact directory (default .omo/spec-alignment)")
    ap.add_argument("--html", action="store_true",
                    help="also write per-document HTML visualizations")
    ap.add_argument("--check-only", action="store_true",
                    help="drift gate only; no coverage artifacts")
    ap.add_argument("--files", nargs="*", default=DEFAULT_GLOBS,
                    help="git ls-files globs to check "
                         "(default: the Rust tree)")
    args = ap.parse_args(argv)

    if not (REPO / CONFIG).exists():
        print(f"error: {CONFIG} not found at repo root", file=sys.stderr)
        return 2
    files = rust_files(REPO, args.files)
    if not files:
        print("error: no source files matched", file=sys.stderr)
        return 2

    out_dir = (REPO / args.out) if not Path(args.out).is_absolute() \
        else Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    cov_path = out_dir / "quotes.coverage"
    cov_path.unlink(missing_ok=True)

    rc, drift = run_check(REPO, out_dir, files, cov_path)
    if drift:
        print("== spec-quote drift ==")
        for m in drift:
            print(f"  {m}")
    if args.check_only:
        return 1 if rc != 0 else 0

    meta = {
        "generated": round(time.time(), 3),
        "git_rev": git_rev(REPO),
        "tool_version": tool_version(),
        "config_sha8": hashlib.sha256(
            (REPO / CONFIG).read_bytes()).hexdigest()[:8],
        "files_checked": len(files),
    }
    try:
        cov = run_coverage(REPO, cov_path)
    except (RuntimeError, json.JSONDecodeError) as exc:
        print(f"error: coverage stage failed: {exc}", file=sys.stderr)
        return 2 if rc == 0 else 1

    stats = [doc_stats(doc) for doc in cov.get("documents", [])]
    summary = {"meta": meta, "drift": drift, "documents": stats,
               "gap_ignores": sorted(f"{s}:{ln}" for s, ln in GAP_IGNORES)}
    (out_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n")
    (out_dir / "SPEC-ALIGNMENT.md").write_text(
        render_markdown(meta, stats, drift))

    if args.html:
        html_dir = out_dir / "html"
        html_dir.mkdir(exist_ok=True)
        # rc 0/1 both valid (1 = gaps exist — see run_coverage)
        proc = run(
            ["greatspectate", "coverage", "--config", CONFIG,
             "--coverage", str(cov_path), "--format", "html",
             "--output-dir", str(html_dir)], REPO)
        if proc.returncode not in (0, 1):
            print(f"warning: html rendering failed: "
                  f"{proc.stderr.strip()[:200]}", file=sys.stderr)

    total_cov = sum(s["covered"] for s in stats)
    total_gap = sum(s["gap"] for s in stats)
    print(f"checked {len(files)} files, {len(stats)} sources: "
          f"{total_cov} covered, {total_gap} gap lines")
    print(f"artifacts: {out_dir}/summary.json, "
          f"{out_dir}/SPEC-ALIGNMENT.md"
          + (f", {out_dir}/html/" if args.html else ""))
    return 1 if rc != 0 else 0


if __name__ == "__main__":
    sys.exit(main())
