#!/usr/bin/env python3
"""Render the perf ledger as a markdown comparison table (readers x metrics).

Reads tests/hardware/perf/results/perf-history.jsonl (append-only JSONL
written by the pytest suite in this directory) and prints, for every
reader, the LATEST recorded row of each metric (max ts per
reader+metric pair — i.e. "the reader's most recent run" even when
runs interleave in the ledger).

Layout:
* a per-reader metadata block (transport, fw_rev, timestamp of the
  latest row, sample counts),
* a p50 matrix and a p99 matrix — readers as rows, metrics as columns,
  the ACR1252 reference pinned as the first row (it anchors "expected
  behavior"; docs/PERF_AND_DRY_STRATEGY.md baseline).

Aggregate metrics (throughput_*_per_sec) only carry p50 — p90/p99/max
are null by schema (an aggregate has no distribution) and render as "-".

Stdlib only; runs without pyscard/pcscd. Usage:

    python3 tests/hardware/perf/report.py                 # latest run per reader
    python3 tests/hardware/perf/report.py --all           # full history dump
    python3 tests/hardware/perf/report.py --out report.md # write to a file
"""

import argparse
import contextlib
import json
import sys
from pathlib import Path

DEFAULT_LEDGER = Path(__file__).resolve().parent / "results" / "perf-history.jsonl"
#: reference row anchor: bench ACR1252 (pcscd name "...ACR1252 Dual Reader PICC...")
REFERENCE_SUBSTR = "ACR1252"

DASH = "-"


def load_rows(path):
    rows, bad = [], 0
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    bad += 1
    except FileNotFoundError:
        print(f"ledger not found: {path} (run the suite first: "
              f"NUCULA_PERF=1 python3 -m pytest tests/hardware/perf/ -v)")
        return []
    if bad:
        print(f"warning: skipped {bad} malformed ledger line(s)", file=sys.stderr)
    return rows


def latest_matrix(rows):
    """{(reader, metric): row} keeping the max-ts row per pair."""
    out = {}
    for r in rows:
        key = (r.get("reader"), r.get("metric"))
        if key not in out or str(r.get("ts", "")) >= str(out[key].get("ts", "")):
            out[key] = r
    return out


def ordered_readers(matrix):
    """Reference reader first, then the rest alphabetically."""
    names = sorted({r for r, _m in matrix})
    ref = [n for n in names if REFERENCE_SUBSTR.lower() in n.lower()]
    return ref + [n for n in names if n not in ref]


def fmt(value, metric):
    if value is None:
        return DASH
    if metric.endswith("_per_sec"):
        return f"{value:,.1f}"
    if metric.endswith("_ms"):
        return f"{value:,.2f}"
    return f"{value:,.3f}"


def metric_columns(matrix):
    return sorted({m for _r, m in matrix})


def markdown_matrix(matrix, readers, metrics, stat):
    header = ["reader"] + [f"{m} {stat}" for m in metrics]
    lines = ["| " + " | ".join(header) + " |",
             "|" + "|".join(["---"] + ["---:" for _ in metrics]) + "|"]
    for name in readers:
        cells = []
        for m in metrics:
            row = matrix.get((name, m))
            cells.append(fmt(row.get(stat) if row else None, m))
        label = name if REFERENCE_SUBSTR.lower() not in name.lower() \
            else f"**{name}** (reference)"
        lines.append("| " + " | ".join([label] + cells) + " |")
    return "\n".join(lines)


def reader_meta(matrix, readers):
    lines = []
    for name in readers:
        rows = [r for (rn, _m), r in matrix.items() if rn == name]
        latest = max(rows, key=lambda r: str(r.get("ts", "")))
        ns = " ".join(f"{r['metric']}=n{r.get('n')}" for r in
                      sorted(rows, key=lambda r: r["metric"]))
        lines.append(f"* {name} — transport={latest.get('transport')} "
                     f"fw_rev={latest.get('fw_rev') or DASH} "
                     f"last={latest.get('ts')} ({ns})")
    return "\n".join(lines)


def render(rows, title):
    matrix = latest_matrix(rows)
    if not matrix:
        print(f"({title}: ledger has no usable rows)")
        return
    readers, metrics = ordered_readers(matrix), metric_columns(matrix)
    print(f"# {title}\n")
    print("## Readers (latest row per reader+metric)\n")
    print(reader_meta(matrix, readers) + "\n")
    print("## p50 (median)\n")
    print(markdown_matrix(matrix, readers, metrics, "p50") + "\n")
    print("## p99 (tail)\n")
    print(markdown_matrix(matrix, readers, metrics, "p99"))


def render_all(rows, _title=None):
    print("# Full ledger history\n")
    print("| ts | reader | transport | metric | n | p50 | p90 | p99 | max |")
    print("|---|---|---|---|---:|---:|---:|---:|---:|")
    for r in sorted(rows, key=lambda r: (str(r.get("ts", "")),
                                         str(r.get("reader", "")))):
        print("| " + " | ".join(str(r.get(k) if r.get(k) is not None else DASH)
                                for k in ("ts", "reader", "transport",
                                          "metric", "n", "p50", "p90",
                                          "p99", "max")) + " |")


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ledger", default=str(DEFAULT_LEDGER),
                    help=f"ledger JSONL path (default: {DEFAULT_LEDGER})")
    ap.add_argument("--all", action="store_true",
                    help="dump every ledger row instead of the latest-run "
                         "comparison tables")
    ap.add_argument("--out", metavar="FILE",
                    help="write the markdown to FILE instead of stdout")
    args = ap.parse_args(argv)

    rows = load_rows(args.ledger)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f, \
                contextlib.redirect_stdout(f):
            (render_all if args.all else render)(rows, "smartcard reader perf")
    else:
        (render_all if args.all else render)(rows, "smartcard reader perf")
    return 0


if __name__ == "__main__":
    sys.exit(main())
