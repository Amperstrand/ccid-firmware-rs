#!/usr/bin/env python3
"""amperstrand.flash-bundle/1 producer (flash-via-labgrid design §4, M1).

Emits <out-root>/<board>/<bundle_id>/ with manifest.json + the target
binaries. bundle_id = sha256 over the canonical JSON of the manifest
with the volatile fields (bundle_id, built_at) removed, keys sorted, no
whitespace — so two consecutive producer runs over the same inputs
yield the SAME bundle_id (design §7 M1 acceptance), while any content
change (git rev, dirty flag, family, layout, target bytes) changes it.

Design note: §4.3 says "manifest with bundle_id removed"; the M1
determinism acceptance ("two consecutive builds of the same rev produce
identical bundle_id") forces built_at out of the canonical set too —
a timestamp that changed every run would break the acceptance by
construction. built_at stays IN the manifest (human truth), OUT of the
hash (identity).

Parsing the partition CSV (design: "20 lines with csv + int(x,0)"):
offsets/sizes come as 0x-prefixed or decimal strings; the app slot is
the first app-type partition named ota_0/factory/app.
"""

from __future__ import annotations

import argparse
import csv
import datetime
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

VOLATILE = ("bundle_id", "built_at")


def canonical_hash(manifest: dict) -> str:
    body = {k: v for k, v in manifest.items() if k not in VOLATILE}
    blob = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(blob).hexdigest()


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def parse_partitions(csv_path: Path) -> tuple[dict, str]:
    """Parse an ESP-IDF partition CSV → ({name: {offset,size}}, app_slot)."""
    parts: dict[str, dict[str, str]] = {}
    app_slot = None
    with csv_path.open(newline="") as fh:
        for row in csv.reader(fh):
            cells = [c.strip() for c in row]
            if not cells or cells[0].startswith("#") or cells[0].lower() == "name":
                continue
            name, ptype = cells[0], cells[1].lower()
            offset = cells[3] if len(cells) > 3 else ""
            size = cells[4] if len(cells) > 4 else ""
            if ptype == "app" and app_slot is None:
                app_slot = name
            if offset:
                parts[name] = {"offset": hex(int(offset, 0)), "size": hex(int(size, 0))}
    if app_slot is None:
        raise SystemExit(f"{csv_path}: no app-type partition found")
    return parts, app_slot


def git_state(repo: Path) -> tuple[str, bool]:
    rev = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, capture_output=True, text=True, check=True
    ).stdout.strip()
    dirty = bool(
        subprocess.run(
            ["git", "status", "--porcelain"], cwd=repo, capture_output=True, text=True, check=True
        ).stdout.strip()
    )
    return rev, dirty


def build_manifest(args: argparse.Namespace) -> dict:
    partitions, app_slot = parse_partitions(Path(args.layout_source))
    targets: dict[str, dict] = {}
    for role, path_str in (
        ("bootloader", args.bootloader),
        ("partition_table", args.partition_table),
        ("app", args.app),
    ):
        if not path_str:
            continue
        p = Path(path_str)
        offset = {
            "bootloader": args.bootloader_offset or "0x0",
            "partition_table": args.partition_table_offset or "0x8000",
            "app": partitions[app_slot]["offset"],
        }[role]
        targets[role] = {
            "offset": hex(int(offset, 0)),
            "path": p.name,
            "sha256": sha256_file(p),
            "bytes": p.stat().st_size,
        }
    if "app" not in targets:
        raise SystemExit("app image is required")
    return {
        "schema": "amperstrand.flash-bundle/1",
        "bundle_id": None,  # filled after canonical hash
        "git_rev": args.git_rev,
        "git_dirty": args.git_dirty,
        "built_at": args.built_at
        or datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "project": args.project,
        "board": args.board,
        "chip": args.chip,
        "family": {
            "esp_idf_version": args.esp_idf_version,
            "sdkconfig_id": sha256_file(Path(args.sdkconfig)) if args.sdkconfig else None,
            "sdkconfig_name": Path(args.sdkconfig).name if args.sdkconfig else None,
            "toolchain": args.toolchain,
        },
        "layout": {
            "source": Path(args.layout_source).name,
            "id": args.layout_id,
            "partitions": partitions,
            "app_slot": app_slot,
        },
        "targets": targets,
        "otadata_policy": args.otadata_policy,
        "boot_signature": args.boot_signature,
        "baud_cap": args.baud_cap,
        "expected_identity": {"usb_iserial": args.usb_iserial},
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--project", required=True)
    ap.add_argument("--board", required=True)
    ap.add_argument("--chip", required=True)
    ap.add_argument("--layout-source", required=True, help="partition CSV path")
    ap.add_argument("--layout-id", required=True)
    ap.add_argument("--app", required=True, help="app binary")
    ap.add_argument("--bootloader")
    ap.add_argument("--partition-table")
    ap.add_argument("--bootloader-offset", default="0x0")
    ap.add_argument("--partition-table-offset", default="0x8000")
    ap.add_argument("--esp-idf-version")
    ap.add_argument("--sdkconfig", help="sdkconfig file (hashed into family)")
    ap.add_argument("--toolchain")
    ap.add_argument("--boot-signature", default=None)
    ap.add_argument("--baud-cap", type=int, default=None)
    ap.add_argument("--usb-iserial", default=None)
    ap.add_argument("--otadata-policy", default="erase-if-family-or-layout-change")
    ap.add_argument("--git-rev")
    ap.add_argument("--git-dirty", action="store_true")
    ap.add_argument("--built-at", help=argparse.SUPPRESS)  # tests only
    ap.add_argument("--out", required=True, help="bundles root dir")
    ap.add_argument("--no-copy", action="store_true", help="manifest only (dry run)")
    args = ap.parse_args(argv)

    if args.git_rev is None:
        args.git_rev, dirty = git_state(Path(__file__).resolve().parent)
        args.git_dirty = args.git_dirty or dirty

    manifest = build_manifest(args)
    manifest["bundle_id"] = canonical_hash(manifest)

    out_dir = Path(args.out) / args.board / manifest["bundle_id"]
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    if not args.no_copy:
        for role, path_str in (
            ("bootloader", args.bootloader),
            ("partition_table", args.partition_table),
            ("app", args.app),
        ):
            if path_str:
                shutil.copy2(path_str, out_dir / Path(path_str).name)

    print(manifest["bundle_id"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
