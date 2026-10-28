#!/usr/bin/env python3
"""Deterministic-field byte-diff for CCID capture pairs (issue #143).

Given two tagged captures of the same card exchanges (same card through
two readers, or the same reader across firmware revs), diff the
DETERMINISTIC fields of each named exchange and ignore the fields the
capture declares random (INITIALIZE UPDATE challenge/cryptogram, card
counters' random-derived bytes, ...) — the generalization of the #132
stage-2 manual byte-diff to a repeatable tool.

Capture format (JSON, one object per file)::

    {
      "reader": "Cherry ST2XXX-001",
      "card": "SmartPGP J3R180 CPLC 50942720",
      "firmware_rev": "7fad866a9",
      "captured_at": "2026-10-28T21:00:00Z",
      "exchanges": [
        {
          "name": "INIT_UPDATE",
          "command": "805000000803cplc...".lower(),
          "response": "000003...9000",
          "random_fields": [
            {"field": "host challenge", "side": "command",
             "offset": 7, "length": 8},
            {"field": "card challenge", "side": "response",
             "offset": 6, "length": 8}
          ]
        }
      ]
    }

* ``command``/``response``: hex strings (case-insensitive, even length).
* ``random_fields``: OPTIONAL per-exchange declarations. ``side`` is
  ``command`` or ``response``; ``offset``/``length`` are byte spans in
  THAT side's buffer. Overlapping spans are unioned. A declaration that
  runs past the end of its buffer is a CAPTURE ERROR (exit 2) — a mask
  must never silently shrink.
* The tag fields (reader/card/firmware_rev/captured_at) describe the
  capture; they are reported, never compared.

Semantics:
  * exchanges are matched BY NAME across the two captures (order-free,
    deterministic output: sorted by name);
  * bytes covered by a random_fields span (present in EITHER capture's
    declaration for that name+side, unless --strict) are MASKED — counted
    and reported, never a difference;
  * any other byte difference, a length difference, or a name present in
    only one capture is a DIFFERENCE.

Exit codes: 0 = captures equal (modulo masks), 1 = differences found,
2 = usage or capture-format error. No hardware, no network — pure host
determinism, CI-able.

Bench doctrine (AGENTS.md card-handling): captures meant for diffing are
the zero-risk read set (SELECT by AID, GET DATA CPLC, applet reads) plus
budgeted singles of SC-class exchanges — the diff tool itself never talks
to a card, but the corpus it consumes must come from disciplined capture
runs.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

SIDES = ("command", "response")


class CaptureError(Exception):
    """Malformed capture (bad hex, bad mask, duplicate name)."""


def parse_hex(name: str, side: str, value) -> bytes:
    if not isinstance(value, str):
        raise CaptureError(f"{name}: {side} is not a string")
    cleaned = value.strip().replace(" ", "").lower()
    if len(cleaned) % 2:
        raise CaptureError(f"{name}: {side} hex has odd length")
    try:
        return bytes.fromhex(cleaned)
    except ValueError as exc:
        raise CaptureError(f"{name}: {side} is not hex: {exc}") from exc


def mask_spans(name: str, exchange: dict) -> dict[str, list[tuple[int, int]]]:
    """Unioned random-field spans per side: [(offset, end), ...]."""
    spans: dict[str, list[tuple[int, int]]] = {side: [] for side in SIDES}
    for decl in exchange.get("random_fields", []) or []:
        side = decl.get("side")
        if side not in SIDES:
            raise CaptureError(
                f"{name}: random field {decl.get('field')!r} has bad side {side!r}"
            )
        offset = decl.get("offset")
        length = decl.get("length")
        if not isinstance(offset, int) or not isinstance(length, int) or length < 1:
            raise CaptureError(f"{name}: random field {decl.get('field')!r} needs offset+length")
        buf = parse_hex(name, side, exchange.get(side, ""))
        if offset + length > len(buf):
            raise CaptureError(
                f"{name}: random field {decl.get('field')!r} mask "
                f"[{offset}..{offset + length}) exceeds {side} length {len(buf)} — "
                "fix the capture declaration, never let a mask shrink"
            )
        spans[side].append((offset, offset + length))
    merged: dict[str, list[tuple[int, int]]] = {}
    for side in SIDES:
        runs: list[tuple[int, int]] = []
        for start, end in sorted(spans[side]):
            if runs and start <= runs[-1][1]:
                runs[-1] = (runs[-1][0], max(runs[-1][1], end))
            else:
                runs.append((start, end))
        merged[side] = runs
    return merged


def load_capture(path: Path) -> dict:
    try:
        data = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise CaptureError(f"{path}: {exc}") from exc
    if not isinstance(data, dict) or not isinstance(data.get("exchanges"), list):
        raise CaptureError(f"{path}: capture must be an object with an 'exchanges' list")
    parsed: dict = {"tags": {k: data.get(k) for k in ("reader", "card", "firmware_rev")}, "ex": {}}
    for ex in data["exchanges"]:
        name = ex.get("name")
        if not isinstance(name, str) or not name:
            raise CaptureError(f"{path}: exchange without a name")
        if name in parsed["ex"]:
            raise CaptureError(f"{path}: duplicate exchange name {name!r}")
        entry = {}
        for side in SIDES:
            if side not in ex:
                raise CaptureError(f"{path}: exchange {name!r} missing {side}")
            entry[side] = parse_hex(f"{path}:{name}", side, ex[side])
        entry["masks"] = mask_spans(f"{path}:{name}", ex)
        parsed["ex"][name] = entry
    return parsed


def union_masked(name_a: str, name_b: str, a: dict, b: dict, strict: bool) -> dict[str, list[tuple[int, int]]]:
    """Effective masks: union of both captures' declarations (or none)."""
    if strict:
        return {side: [] for side in SIDES}
    out: dict[str, list[tuple[int, int]]] = {}
    for side in SIDES:
        runs = sorted(a["masks"][side] + b["masks"][side])
        merged: list[tuple[int, int]] = []
        for start, end in runs:
            if merged and start <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(merged[-1][1], end))
            else:
                merged.append((start, end))
        out[side] = merged
    return out


def covered(spans: list[tuple[int, int]], index: int) -> bool:
    return any(start <= index < end for start, end in spans)


def diff_captures(a: dict, b: dict, strict: bool = False) -> tuple[list[str], int]:
    """Returns (report lines, diff count). Deterministic order."""
    lines: list[str] = []
    diffs = 0
    names = sorted(set(a["ex"]) | set(b["ex"]))
    for name in names:
        ea, eb = a["ex"].get(name), b["ex"].get(name)
        if ea is None or eb is None:
            where = "only in capture B" if ea is None else "only in capture A"
            lines.append(f"DIFF {name}: exchange {where}")
            diffs += 1
            continue
        masks = union_masked(name, name, ea, eb, strict=strict)
        for side in SIDES:
            ba, bb = ea[side], eb[side]
            masked = 0
            side_diffs: list[tuple[int, int, int]] = []
            for i, (xa, xb) in enumerate(zip(ba, bb)):
                if covered(masks[side], i):
                    masked += 1
                elif xa != xb:
                    side_diffs.append((i, xa, xb))
            if len(ba) != len(bb):
                lines.append(
                    f"DIFF {name}.{side}: length {len(ba)} vs {len(bb)}"
                    + (f" ({masked} masked bytes ignored)" if masked else "")
                )
                diffs += 1
            if side_diffs:
                lines.append(
                    f"DIFF {name}.{side}: {len(side_diffs)} deterministic byte(s) differ"
                    + (f" ({masked} masked)" if masked else "")
                )
                for i, xa, xb in side_diffs[:10]:
                    lines.append(f"    [{i:3d}] {xa:02X} vs {bb[i]:02X}")
                if len(side_diffs) > 10:
                    lines.append(f"    ... {len(side_diffs) - 10} more")
                diffs += 1
            if not side_diffs and len(ba) == len(bb) and masked:
                lines.append(f"OK   {name}.{side}: equal modulo {masked} masked byte(s)")
    return lines, diffs


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("capture_a", type=Path)
    parser.add_argument("capture_b", type=Path)
    parser.add_argument(
        "--strict",
        action="store_true",
        help="ignore random_fields declarations and compare raw bytes "
        "(audits the declarations themselves)",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="machine-readable output: one JSON object with tags + diffs",
    )
    args = parser.parse_args(argv)

    try:
        cap_a = load_capture(args.capture_a)
        cap_b = load_capture(args.capture_b)
    except CaptureError as exc:
        print(f"capture error: {exc}", file=sys.stderr)
        return 2

    lines, diffs = diff_captures(cap_a, cap_b, strict=args.strict)
    if args.json:
        print(
            json.dumps(
                {
                    "a": cap_a["tags"],
                    "b": cap_b["tags"],
                    "strict": args.strict,
                    "equal": diffs == 0,
                    "report": lines,
                },
                indent=2,
                sort_keys=True,
            )
        )
    else:
        for tag, cap in (("A", cap_a), ("B", cap_b)):
            print(f"capture {tag}: {cap['tags']}")
        for line in lines:
            print(line)
        print("VERDICT:", "EQUAL (modulo declared random fields)" if diffs == 0 else f"{diffs} difference(s)")
    return 0 if diffs == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
