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

Producer-side self-consistency (design §4.4 — the flasher re-verifies
all of this via tools/hil/flash_bundle.py `check`/`verify`):
  * check 4 — the app image header must be magic 0xE9 + flash-mode DIO
    (issue #309, the build.sh guard moved into the bundle check);
  * check 5 — a bootloader target requires family.esp_idf_version;
  * check 6 — every ESP32 bundle carries a bootloader target;
  * check 7 — the bootloader offset is DERIVED from the chip (esp32 →
    0x1000, C3/S3/C2 → 0x0), never a caller constant.
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

# Bootloader flash offset per chip (design §4.4 check 7): DERIVED from the
# chip family, never hardcoded by the caller. Classic xtensa ESP32 boots its
# bootloader at 0x1000; the C3/S3/C2 generations at 0x0. A bootloader written
# at the WRONG one of these is the 2026-10-08 m5stick incident (bootloader at
# the C3 offset 0x0 on a classic ESP32 → `flash read err, 1000` boot loop).
BOOTLOADER_OFFSETS = {
    "esp32": "0x1000",
    "esp32c3": "0x0",
    "esp32s3": "0x0",
    "esp32c2": "0x0",
}

ESP_CHIPS = tuple(BOOTLOADER_OFFSETS)

# ESP-IDF image header (esp_image_header_t): byte 0 = 0xE9 magic, byte 2 =
# flash mode. 0x02 = DIO — the only mode this repo's sdkconfigs mandate
# (issue #309: a QIO-header app silently boot-loops every DIO board).
ESP_IMAGE_MAGIC = 0xE9
FLASH_MODE_DIO = 0x02


def bootloader_offset_for(chip: str) -> str:
    """The §4.4 check-7 rule. Unknown chip → refuse (no rule, no guess)."""
    try:
        return BOOTLOADER_OFFSETS[chip]
    except KeyError:
        known = ", ".join(f"{c}→{o}" for c, o in sorted(BOOTLOADER_OFFSETS.items()))
        raise SystemExit(
            f"unknown chip {chip!r}: no bootloader-offset rule (design §4.4 "
            f"check 7 — offsets are derived, never guessed; known: {known})"
        )


def check_flash_mode_dio(app: Path, chip: str) -> None:
    """§4.4 check 4 — the build.sh #309 guard, moved into the bundle check."""
    if not chip.startswith("esp32"):
        return  # STM32 (and friends) have no ESP image header
    hdr = app.open("rb").read(3)
    if len(hdr) < 3:
        raise SystemExit(f"{app}: too short for an ESP image header (issue #309 guard)")
    magic, _, mode = hdr[0], hdr[1], hdr[2]
    if magic != ESP_IMAGE_MAGIC:
        raise SystemExit(
            f"{app}: image magic is 0x{magic:02x} (expected 0xE9) — not an ESP app image"
        )
    if mode != FLASH_MODE_DIO:
        raise SystemExit(
            f"{app}: flash-mode byte is 0x{mode:02x} (expected 0x02/dio) — "
            "refusing (issue #309: QIO-header apps boot-loop DIO boards)"
        )


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
    is_esp = args.chip.startswith("esp32")
    if is_esp:
        # §4.4 check 7: bootloader offset is DERIVED from the chip, never a
        # caller guess. An explicit --bootloader-offset may only AGREE.
        derived_bl = bootloader_offset_for(args.chip)
        if args.bootloader_offset is not None and hex(int(args.bootloader_offset, 0)) != derived_bl:
            raise SystemExit(
                f"--bootloader-offset {args.bootloader_offset} contradicts chip "
                f"{args.chip} (design §4.4 check 7: {args.chip} → {derived_bl}, "
                "derived — drop the flag)"
            )
        # §4.4 check 6: every ESP32 bundle carries a bootloader target. A
        # producer that "doesn't build one" cannot emit a bundle — keeping
        # whatever bootloader happens to be installed recreates the
        # v5.5.1-under-v5.5.5 mixed-generation incident.
        if not args.bootloader:
            raise SystemExit(
                "ESP32 bundle without a bootloader target refused (design §4.4 "
                "check 6): a bundle is a complete reprovision set"
            )
    if args.layout_source:
        if not args.layout_id:
            raise SystemExit("--layout-id is required with --layout-source")
        partitions, app_slot = parse_partitions(Path(args.layout_source))
        layout = {
            "source": Path(args.layout_source).name,
            "id": args.layout_id,
            "partitions": partitions,
            "app_slot": app_slot,
        }
        app_offset = partitions[app_slot]["offset"]  # §4.4 check 2, by construction
    elif is_esp:
        raise SystemExit("--layout-source (the partition CSV) is required for ESP32 bundles")
    else:
        # STM32 shape (design §4.2): single app target, layout null.
        layout = None
        app_offset = hex(int(args.app_offset, 0))
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
            "bootloader": bootloader_offset_for(args.chip) if is_esp else args.bootloader_offset,
            "partition_table": args.partition_table_offset or "0x8000",
            "app": app_offset,
        }[role]
        targets[role] = {
            "offset": hex(int(offset, 0)),
            "path": p.name,
            "sha256": sha256_file(p),
            "bytes": p.stat().st_size,
        }
    if "app" not in targets:
        raise SystemExit("app image is required")
    if "bootloader" in targets and not args.esp_idf_version:
        # §4.4 check 5: a bootloader target without its generation stamp is
        # unverifiable — the two-bootloader-generations hazard IS this field.
        raise SystemExit(
            "--esp-idf-version is required when a bootloader is a target "
            "(design §4.4 check 5 — family.esp_idf_version is the "
            "bootloader-generation identity)"
        )
    check_flash_mode_dio(Path(args.app), args.chip)
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
        "layout": layout,
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
    ap.add_argument("--chip", required=True,
                    help="esp32 | esp32c3 | esp32s3 | esp32c2 | stm32* (decides "
                         "the bootloader offset, design §4.4 check 7)")
    ap.add_argument("--layout-source", help="partition CSV path (required for ESP32)")
    ap.add_argument("--layout-id", help='stable layout name, e.g. "ota:v1" (required with --layout-source)')
    ap.add_argument("--app", required=True, help="app binary")
    ap.add_argument("--app-offset", default="0x08000000",
                    help="app offset for layout-less (STM32) bundles [0x08000000]")
    ap.add_argument("--bootloader", help="bootloader binary (MANDATORY for ESP32, §4.4 check 6)")
    ap.add_argument("--partition-table")
    ap.add_argument("--bootloader-offset",
                    help="may only AGREE with the chip-derived offset (§4.4 check 7); drop it")
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
