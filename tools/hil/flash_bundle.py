#!/usr/bin/env python3
"""flash_bundle.py — amperstrand.flash-bundle/1 verifier + CLI (design §4.4–4.5).

The flash-side half of the flash-via-labgrid bundle tooling (kanban
t_7a2ec5c3; producer = tools/hil/make_bundle.py, M1 #425). Software
only: no labgrid, no hardware, no serial — this module decides WHETHER
a flash plan is safe and emits the offsets; esp32_flash.py (M2) will
consume the plan under the bench flock + place.

Doctrine (design §2): bundle-or-nothing — mixed-family/mixed-layout
flash sets are refused STRUCTURALLY, not by operator care. Every check
here is a replay of an incident:

  * 0x30000-is-the-factory-app (2026-10-20): build.sh's hardcoded
    otadata erase at 0x30000 lands on the RUNNING FACTORY APP under the
    micronuts table. Refused here three ways: erase offsets come only
    from the bundle's parsed layout (never a parameter default), a
    requested erase must BE the layout's otadata, and an app-only
    erase plan may never overlap a device app slot.
  * two bootloader generations (v5.5.1-on-flash under v5.5.5 images):
    family.esp_idf_version IS the generation identity; an app-only
    flash against a different generation is refused.
  * bootloader-at-wrong-offset (2026-10-08 m5stick): re-derived from
    the chip on every check.

Refusal matrix (design §4.5; rows match top-down, full bundle WINS over
unknown inventory — it is the seed path):

| request            | inventory            | verdict                        |
|--------------------|----------------------|--------------------------------|
| full bundle        | anything / unknown   | FLASH (only family/layout path)|
| app-only, same id  | same bundle_id       | FLASH (idempotent, no erase)   |
| app-only, newer rev| family+layout match  | FLASH (no erase)               |
| app-only           | family OR layout ≠   | REFUSE family/layout-mismatch  |
| app-only           | unknown / stale      | REFUSE unknown-device          |
|                    |                      |  (unless --allow-unknown-      |
|                    |                      |   inventory)                   |
| erase at X         | X ∉ bundle otadata   | REFUSE otadata-offset-not-in-  |
|                    |                      |  layout (the 0x30000 trap)     |
| app-only w/ erase  | erase hits device    | REFUSE erase-overlaps-device-  |
|                    | app slot             |  app                           |
| PT-only / bl-only  | anything             | REFUSE partial-structural-     |
|                    |                      |  targets                       |

Modes:
  build ...               producer passthrough (see make_bundle.py)
  check <bundle-dir>      §4.4 self-consistency; exit 1 on any failure
  verify <bundle-dir> ... §4.5 refusal matrix against a device
                         inventory entry; exit 0 bless / 1 refuse

Exit codes: 0 ok/blessed, 1 refused/failed, 2 usage.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

HIL_TOOLS_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(HIL_TOOLS_DIR))
import make_bundle  # noqa: E402  (same-dir import; the producer authority)

SCHEMA = "amperstrand.flash-bundle/1"
INVENTORY_SCHEMA = "amperstrand.device-inventory/1"
STRUCTURAL_TARGETS = ("bootloader", "partition_table")

# Design §4.5 precedence: evaluated in this order; first refusal wins.
REFUSAL_ORDER = (
    "schema-unknown",
    "file-missing",
    "missing-digest",
    "digest-mismatch",
    "app-offset-not-in-slot",
    "layout-csv-mismatch",
    "bad-flash-mode",
    "missing-family",
    "missing-bootloader",
    "bootloader-offset-mismatch",
    "partial-structural-targets",
    "otadata-offset-not-in-layout",
    "unknown-device",
    "family-mismatch",
    "layout-mismatch",
    "erase-overlaps-device-app",
)


def _refuse(reason: str, detail: str, extra: dict | None = None) -> dict:
    v = {"verdict": "refuse", "reason": reason, "detail": detail}
    if extra:
        v.update(extra)
    return v


def _bless(plan: dict, warnings: list[str] | None = None) -> dict:
    return {"verdict": "flash", "plan": plan, "warnings": warnings or []}


def load_manifest(bundle_dir: Path) -> dict:
    return json.loads((bundle_dir / "manifest.json").read_text())


def load_inventory(path: Path) -> dict:
    inv = json.loads(path.read_text())
    schema = inv.get("schema")
    if schema != INVENTORY_SCHEMA:
        raise SystemExit(
            f"{path}: schema {schema!r} != {INVENTORY_SCHEMA!r} — refusing to "
            "guess at inventory semantics"
        )
    return inv


def _ranges_overlap(a_off: int, a_size: int, b_off: int, b_size: int) -> bool:
    return a_off < b_off + b_size and b_off < a_off + a_size


def _partitions_map(layout: dict | None) -> dict:
    return dict(layout or {}).get("partitions") or {}


def check_bundle(manifest: dict, bundle_dir: Path,
                 csv_path: Path | None = None) -> list[tuple[str, str]]:
    """Design §4.4 self-consistency, producer AND flasher re-verify.

    Returns a list of (reason, detail) failures — empty = consistent.
    """
    fails: list[tuple[str, str]] = []
    chip = manifest.get("chip", "")
    is_esp = chip.startswith("esp32")
    targets = manifest.get("targets") or {}
    layout = manifest.get("layout")

    # check 1 — every target file exists next to the manifest with its
    # recorded sha256 and byte count.
    for role, t in targets.items():
        p = bundle_dir / t["path"]
        if not p.is_file():
            fails.append(("file-missing", f"targets.{role}: {t['path']} not in bundle"))
            continue
        if not t.get("sha256"):
            fails.append(("missing-digest", f"targets.{role}: no sha256 recorded"))
            continue
        digest = make_bundle.sha256_file(p)
        if digest != t["sha256"]:
            fails.append(("digest-mismatch",
                          f"targets.{role}: sha256 {digest[:12]}… != recorded {t['sha256'][:12]}…"))
        elif p.stat().st_size != t.get("bytes"):
            fails.append(("digest-mismatch",
                          f"targets.{role}: {p.stat().st_size} bytes != recorded {t.get('bytes')}"))

    # check 2 — the app lands in a slot the flashed table defines.
    if is_esp and layout and "app" in targets:
        want = _partitions_map(layout).get(layout.get("app_slot"), {}).get("offset")
        if want is None or int(targets["app"]["offset"], 0) != int(want, 0):
            fails.append(("app-offset-not-in-slot",
                          f"app@{targets['app']['offset']} != layout slot "
                          f"{layout.get('app_slot')}@{want}"))

    # check 3 — a partition-table target implies a layout; a CSV shipped
    # alongside is re-parsed and must reproduce the recorded layout.
    if "partition_table" in targets and not layout:
        fails.append(("layout-csv-mismatch",
                      "partition_table target without a parsed layout"))
    if csv_path is None:
        shipped = sorted(bundle_dir.glob("*.csv"))
        csv_path = shipped[0] if len(shipped) == 1 else None
    if csv_path is not None and layout:
        parts, app_slot = make_bundle.parse_partitions(csv_path)
        if parts != _partitions_map(layout) or app_slot != layout.get("app_slot"):
            fails.append(("layout-csv-mismatch",
                          f"{csv_path.name} re-parse != recorded layout (check 3)"))

    # check 4 — ESP32 app image header: magic 0xE9 + flash-mode DIO (#309).
    if is_esp and "app" in targets and (bundle_dir / targets["app"]["path"]).is_file():
        hdr = (bundle_dir / targets["app"]["path"]).open("rb").read(3)
        if len(hdr) < 3 or hdr[0] != make_bundle.ESP_IMAGE_MAGIC or hdr[2] != make_bundle.FLASH_MODE_DIO:
            mode = f"0x{hdr[2]:02x}" if len(hdr) == 3 else "no-header"
            fails.append(("bad-flash-mode",
                          f"app image flash-mode byte is {mode} (expected 0x02/dio, #309)"))

    # check 5 — a bootloader target carries its generation stamp.
    if "bootloader" in targets and not (manifest.get("family") or {}).get("esp_idf_version"):
        fails.append(("missing-family",
                      "bootloader target without family.esp_idf_version "
                      "(the bootloader-generation identity)"))

    # check 6 — every ESP32 bundle is a complete reprovision set.
    if is_esp and "bootloader" not in targets:
        fails.append(("missing-bootloader",
                      "ESP32 bundle without a bootloader target (a bundle must "
                      "carry bl+PT+app from one family)"))
    if is_esp and set(targets) != {"bootloader", "partition_table", "app"}:
        fails.append(("partial-structural-targets",
                      f"ESP32 bundle targets are {sorted(targets)} — the manifest "
                      "must carry the complete bl+PT+app set (§4.5: never split "
                      "the structural pair from the app)"))
    if not is_esp and set(targets) != {"app"}:
        fails.append(("partial-structural-targets",
                      f"STM32 bundle targets are {sorted(targets)} — expected "
                      "exactly ['app'] (design §4.2)"))

    # check 7 — bootloader offset re-derived from the chip.
    if is_esp and "bootloader" in targets:
        derived = make_bundle.bootloader_offset_for(chip)
        if int(targets["bootloader"]["offset"], 0) != int(derived, 0):
            fails.append(("bootloader-offset-mismatch",
                          f"bootloader@{targets['bootloader']['offset']} but chip "
                          f"{chip} derives {derived}"))

    fails.sort(key=lambda f: REFUSAL_ORDER.index(f[0]) if f[0] in REFUSAL_ORDER else 99)
    return fails


def verify(manifest: dict, bundle_dir: Path, device: str,
           inventory: dict, requested_targets: set[str] | None = None,
           erase_otadata_at: str | None = None,
           allow_unknown_inventory: bool = False) -> dict:
    """The §4.5 refusal matrix. Bless => {"verdict": "flash", "plan": …};
    refuse => {"verdict": "refuse", "reason": <stable code>, "detail": …}."""
    if manifest.get("schema") != SCHEMA:
        return _refuse("schema-unknown", f"manifest schema {manifest.get('schema')!r}")
    fails = check_bundle(manifest, bundle_dir)
    if fails:
        reason, detail = fails[0]
        return _refuse(reason, detail, {"all_failures": [f"{r}: {d}" for r, d in fails]})

    targets = manifest["targets"]
    layout = manifest.get("layout")
    chip = manifest["chip"]
    is_esp = chip.startswith("esp32")
    requested = requested_targets or set(targets)
    unknown = requested - set(targets)
    if unknown:
        return _refuse("partial-structural-targets",
                       f"requested targets {sorted(unknown)} not in bundle")

    full = set(targets) <= requested  # every structural target + app present
    app_only = requested == {"app"}
    if not full and not app_only:
        # PT-only, bootloader-only, bl+PT-without-app: never split the
        # structural pair from the app (§4.5 last row).
        return _refuse("partial-structural-targets",
                       f"requested {sorted(requested)} — structural targets "
                       "never flash without the full set; use a full bundle")

    # Erase plan — offsets ONLY from the bundle's parsed layout (§3.1:
    # "erase otadata (offset FROM THE BUNDLE PT) — never hardcoded").
    otadata = _partitions_map(layout).get("otadata")
    derived_erase = (int(otadata["offset"], 0), int(otadata["size"], 0)) if otadata else None
    if erase_otadata_at is not None:
        want = int(erase_otadata_at, 0)
        if derived_erase is None or want != derived_erase[0]:
            return _refuse(
                "otadata-offset-not-in-layout",
                f"erase@{hex(want)} is not this bundle layout's otadata "
                f"({hex(derived_erase[0]) if derived_erase else 'none defined'}) "
                "— the 0x30000-is-the-factory-app trap (design §4.5)",
            )
        if not app_only:  # full bundle erases its own otadata anyway
            derived_erase = (want, derived_erase[1])

    entry = (inventory.get("devices") or {}).get(device)
    warnings: list[str] = []

    # §4.5 precedence: the full-bundle row WINS over the unknown-inventory
    # row — a full bundle is the only path allowed to change family/layout
    # and IS the first-adoption/seed path (the flash itself seeds the
    # inventory). App-only shapes can never bootstrap a device, so they
    # reconcile strictly.
    if app_only and not full:
        if entry is None:
            if not allow_unknown_inventory:
                return _refuse(
                    "unknown-device",
                    f"device {device!r} not in inventory — app-only cannot "
                    "bootstrap a device; flash a full bundle (the seed path) "
                    "or pass --allow-unknown-inventory",
                )
            warnings.append(f"inventory unknown for {device!r} — allowed by flag")
        else:
            dev_family = entry.get("family") or {}
            fam = manifest.get("family") or {}
            if entry.get("bundle_id") == manifest["bundle_id"]:
                pass  # idempotent reflash — same id wins before family compare
            else:
                # esp_idf_version is the bootloader-GENERATION identity
                # (§1.6: v5.5.1-on-flash under v5.5.5 images) — strict.
                if dev_family.get("esp_idf_version") != fam.get("esp_idf_version"):
                    return _refuse(
                        "family-mismatch",
                        f"device carries family.esp_idf_version="
                        f"{dev_family.get('esp_idf_version')!r} (project "
                        f"{entry.get('project')!r}); bundle carries "
                        f"{fam.get('esp_idf_version')!r} — bootloader-generation "
                        "conflict; flash a full bundle or --reprovision",
                    )
                # sdkconfig_id refines the family; UNKNOWN (null, e.g. a
                # seed row from forensics) warns rather than refuses.
                dev_sdk = dev_family.get("sdkconfig_id")
                cur_sdk = fam.get("sdkconfig_id")
                if dev_sdk is not None and cur_sdk is not None and dev_sdk != cur_sdk:
                    return _refuse(
                        "family-mismatch",
                        f"device carries family.sdkconfig_id={str(dev_sdk)[:12]}…; "
                        f"bundle carries {str(cur_sdk)[:12]}… — same IDF version, "
                        "different sdkconfig; flash a full bundle or --reprovision",
                    )
                if dev_sdk is None or cur_sdk is None:
                    warnings.append(
                        "device-or-bundle sdkconfig_id unknown — family verified "
                        "on esp_idf_version only"
                    )
                dev_layout = entry.get("layout") or {}
                dev_parts = dev_layout.get("app_partitions") or {}
                same_layout = entry.get("layout_id") == (layout or {}).get("id")
                if same_layout and dev_parts and is_esp:
                    bundle_parts = _partitions_map(layout)
                    for name, rng in dev_parts.items():
                        b = bundle_parts.get(name)
                        if b and (int(b["offset"], 0) != int(rng["offset"], 0)
                                  or int(b["size"], 0) != int(rng["size"], 0)):
                            same_layout = False
                            warnings.append(
                                f"device layout row {name} differs from bundle "
                                "layout under the same id — treating as mismatch"
                            )
                if not same_layout:
                    return _refuse(
                        "layout-mismatch",
                        f"device carries layout {entry.get('layout_id')!r}; bundle "
                        f"carries {(layout or {}).get('id')!r} — erase/write offsets "
                        "from this bundle's table would hit the other layout's "
                        "slots (the 0x30000-is-the-factory-app class); flash a "
                        "full bundle or --reprovision",
                    )
        # app-only + an erase in the plan: the device's table STAYS, so
        # the erase may never overlap a device app slot.
        if entry is not None and (erase_otadata_at is not None):
            dev_parts = (entry.get("layout") or {}).get("app_partitions") or {}
            if derived_erase is not None:
                e_off, e_size = derived_erase
                for name, rng in dev_parts.items():
                    if _ranges_overlap(e_off, e_size, int(rng["offset"], 0), int(rng["size"], 0)):
                        return _refuse(
                            "erase-overlaps-device-app",
                            f"erase [{hex(e_off)}..{hex(e_off + e_size)}) overlaps "
                            f"device app partition {name}@{rng['offset']}+{rng['size']} "
                            "— refusing to erase a running app slot",
                        )

    # BLESS — assemble the flash plan esp32_flash.py will execute:
    # images in ONE esptool invocation, offsets DESCENDING (writes before
    # erases, PT lands last), erases only after all writes.
    images = sorted(
        ({"offset": t["offset"],
          "path": str((bundle_dir / t["path"]).resolve()),
          "sha256": t["sha256"]}
         for role, t in targets.items() if role in requested),
        key=lambda im: -int(im["offset"], 0),
    )
    erases = []
    if full and derived_erase is not None and manifest.get("otadata_policy") != "never":
        erases.append({"offset": hex(derived_erase[0]), "size": hex(derived_erase[1]),
                       "why": "otadata_policy: full bundle (re)provisions boot selection"})
    elif app_only and erase_otadata_at is not None and derived_erase is not None:
        erases.append({"offset": hex(derived_erase[0]), "size": hex(derived_erase[1]),
                       "why": "requested otadata erase (validated against the bundle layout)"})
    plan = {
        "device": device,
        "bundle_id": manifest["bundle_id"],
        "chip": chip,
        "targets": sorted(requested),
        "images": images,
        "erases": erases,
        "boot_signature": manifest.get("boot_signature"),
        "boot_signature_reject": manifest.get("boot_signature_reject"),
        "baud_cap": manifest.get("baud_cap"),
        "expected_identity": manifest.get("expected_identity"),
    }
    if entry is None:
        warnings.append("device not in inventory — this flash seeds it (full-bundle path)")
    return _bless(plan, warnings)


def _cmd_check(args: argparse.Namespace) -> int:
    bundle_dir = Path(args.bundle_dir)
    try:
        manifest = load_manifest(bundle_dir)
    except (OSError, json.JSONDecodeError) as e:
        print(f"REFUSE schema-unknown: cannot read manifest ({e})")
        return 1
    if manifest.get("schema") != SCHEMA:
        print(f"REFUSE schema-unknown: {manifest.get('schema')!r}")
        return 1
    fails = check_bundle(manifest, bundle_dir, Path(args.csv) if args.csv else None)
    if fails:
        for reason, detail in fails:
            print(f"REFUSE {reason}: {detail}")
        return 1
    print(f"OK {manifest['bundle_id'][:12]}… — §4.4 checks 1-7 clean")
    return 0


def _cmd_verify(args: argparse.Namespace) -> int:
    bundle_dir = Path(args.bundle_dir)
    try:
        manifest = load_manifest(bundle_dir)
        inventory = load_inventory(Path(args.inventory))
    except (OSError, json.JSONDecodeError, SystemExit) as e:
        print(f"REFUSE schema-unknown: {e}")
        return 1
    requested = set(args.targets) if args.targets else None
    v = verify(manifest, bundle_dir, args.device, inventory,
               requested_targets=requested,
               erase_otadata_at=args.erase_otadata_at,
               allow_unknown_inventory=args.allow_unknown_inventory)
    if args.json:
        print(json.dumps(v, indent=2))
    if v["verdict"] == "refuse":
        if not args.json:
            print(f"REFUSE {v['reason']}: {v['detail']}")
        return 1
    if not args.json:
        print(f"FLASH {v['plan']['bundle_id'][:12]}… -> {args.device} "
              f"(targets: {', '.join(v['plan']['targets'])})")
        for im in v["plan"]["images"]:
            print(f"  write {im['offset']}  {Path(im['path']).name}  ({im['sha256'][:12]}…)")
        for er in v["plan"]["erases"]:
            print(f"  erase {er['offset']} +{er['size']}  ({er['why']})")
        for w in v["warnings"]:
            print(f"  warning: {w}")
    return 0


def main(argv: list[str] | None = None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    # `build` passthrough — intercepted BEFORE argparse (REMAINDER chokes
    # on a leading --option token; the producer owns its own CLI).
    if argv and argv[0] == "build":
        return make_bundle.main([a for a in argv[1:] if a != "--"])

    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("build", help="produce a bundle (see make_bundle.py)")

    c = sub.add_parser("check", help="§4.4 self-consistency checks")
    c.add_argument("bundle_dir")
    c.add_argument("--csv", help="partition CSV to re-parse against the manifest layout")
    c.set_defaults(func=_cmd_check)

    v = sub.add_parser("verify", help="§4.5 refusal matrix vs a device inventory entry")
    v.add_argument("bundle_dir")
    v.add_argument("--device", required=True, help="device id in the inventory")
    v.add_argument("--inventory", required=True,
                   help=f"device-inventory JSON ({INVENTORY_SCHEMA})")
    v.add_argument("--targets", action="append",
                   help="restrict request (e.g. --targets app); default: all")
    v.add_argument("--erase-otadata-at", metavar="OFFSET",
                   help="requested otadata erase offset — must BE the bundle "
                        "layout's otadata (replaces build.sh's hardcoded 0x30000)")
    v.add_argument("--allow-unknown-inventory", action="store_true")
    v.add_argument("--json", action="store_true", help="machine verdict JSON")
    v.set_defaults(func=_cmd_verify)

    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
