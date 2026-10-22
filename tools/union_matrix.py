#!/usr/bin/env python3
"""union_matrix.py — ONE source of truth for the esp32-ccid union compile matrix.

Kanban t_f579fc05 (union matrix CI): the valid backend x board x transport x
mode combos are PARSED LIVE from firmware/esp32-ccid/build.sh — the bench
flow is the authority — plus a small static table of combos that exist only
in CI. Because the workflow generates its matrix from this script
(`--format matrix`), a new build.sh case can never be silently skipped:
it lands in the matrix the moment it lands in build.sh, and an invalid
entry (feature that no longer exists, sdkconfig that was renamed) fails
LOUDLY here (`check` mode) at the construction seam instead of rotting a
matrix leg.

Modes:
  check                     validate everything (features exist in
                            firmware/esp32-ccid/Cargo.toml, sdkconfig files
                            exist, build.sh parses, selftest) — exit 1 on drift
  selftest                  parse a synthetic fixture; assert the parser's
                            invariants (used by CI and by humans editing this)
  --format matrix           GitHub `matrix.include` JSON (compact) for the
                            union-matrix workflow
  --format host-matrix      GitHub matrix JSON for the HOST-side (x86_64)
                            backend x board check legs
  --format table            human-readable combo table (dims per combo)
  --format commands         local entrypoint: exact cargo incantation per
                            variant (env exports included) — the make-style
                            equivalent of one CI leg
  --variant <name>          restrict table/commands output to one variant
  --filter <substr>         restrict output to variants whose name contains
                            substr (workflow_dispatch smoke runs)

Exit codes: 0 ok, 1 validation failure / unknown variant / empty result.
"""

from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
BUILD_SH = REPO_ROOT / "firmware" / "esp32-ccid" / "build.sh"
FW_DIR = REPO_ROOT / "firmware" / "esp32-ccid"
FW_MANIFEST = FW_DIR / "Cargo.toml"

# Combos that exist ONLY in CI (no build.sh case). Provenance recorded per
# entry — extend this table, never ci.yml inline, so the manifest stays the
# single place the matrix is defined.
CI_ONLY_COMBOS = [
    {
        # ci.yml esp32-build-secondary `pn532` leg: the PN532 frontend has
        # no bench variant; this is its only compile coverage today.
        "name": "pn532-m5atom",
        "features": "backend-pn532",
        "no_default_features": True,
        "target": "xtensa-esp32-espidf",
        "sdkconfig": "sdkconfig.defaults;sdkconfig.defaults.esp32",
        "sdkconfig_full": False,
        "provenance": "ci.yml esp32-build-secondary",
    },
]

# HOST-side (x86_64) backend x board x transport combos — the cheap half of
# the union matrix: no IDF, no esp toolchain, pure feature-combination
# compile checks of the esp32-ccid crate (host target compiles the shared
# logic; board/frontend specifics are target-gated). Valid pairs only:
# backend x board unions that a real variant uses (build.sh or Cargo.toml
# documented composition, e.g. the wifi-vpcd comment listing the valid
# vpcd pairs). An INVALID union here (e.g. pn532 x vpcd) must be left out —
# adding it would test a combination nothing ships.
HOST_COMBOS = [
    {"name": "host-mfrc522-m5atom", "features": "backend-mfrc522,board-m5atom"},
    {"name": "host-mfrc522-m5stick", "features": "backend-mfrc522,board-m5stick"},
    {"name": "host-pn532", "features": "backend-pn532"},
    {"name": "host-pn7160-nucula", "features": "backend-pn7160,board-nucula"},
    {"name": "host-vpcd-mfrc522-m5atom", "features": "wifi-vpcd,backend-mfrc522,board-m5atom"},
]

# Dependency allowlist of the board-blind serving kernel (t_72e1e2ed
# boundary; crates/ccid-service/Cargo.toml documents the rule). The union
# workflow's boundary job asserts ccid-service never grows a dep outside
# this set — "works on any board" stays compiler-enforced.
CCID_SERVICE_ALLOWED_DEPS = {
    "card-interface",
    "ccid-core",
    "ccid-protocol",
    "ccid-transport-serial",
    "heapless",
    "log",
}

CASE_RE = re.compile(r"^    ([a-z0-9-]+)\)$", re.MULTILINE)
FIELD_RES = {
    "triple": re.compile(r'TRIPLE="([^"]+)"'),
    "sdkconfig": re.compile(r'SDKCONFIG="\$\{SCRIPT_DIR\}/([^"/]+)"'),
    # FEATURE_ARGS comes in three shapes: (a) empty (), (b) a plain list,
    # (c) a quoted list containing ${BOARD} (substituted with the case name).
    "features": re.compile(
        r"FEATURE_ARGS=\(--no-default-features --features (?:\"([^\"]*)\"|([^)\n]+))\)"
    ),
}


def parse_build_sh(text: str) -> list[dict]:
    """Parse the `case "${BOARD}" in` block of build.sh into combo dicts."""
    start = text.index('case "${BOARD}" in')
    end = text.index("\nesac", start)
    block = text[start:end]
    cases: list[dict] = []
    matches = list(CASE_RE.finditer(block))
    for i, m in enumerate(matches):
        name = m.group(1)
        if name == "*":
            continue  # usage/error arm
        chunk = block[m.end(): matches[i + 1].start() if i + 1 < len(matches) else len(block)]
        triple = FIELD_RES["triple"].search(chunk)
        sdk = FIELD_RES["sdkconfig"].search(chunk)
        feats = FIELD_RES["features"].search(chunk)
        if not triple or not sdk:
            raise ValueError(f"build.sh case {name!r}: missing TRIPLE or SDKCONFIG")
        if feats:
            raw = feats.group(1) if feats.group(1) is not None else feats.group(2)
            features = raw.replace("${BOARD}", name).strip()
            no_default = True
        else:
            # FEATURE_ARGS=() — default features (m5atom flow)
            if "FEATURE_ARGS=()" not in chunk:
                raise ValueError(f"build.sh case {name!r}: no parseable FEATURE_ARGS")
            features = ""
            no_default = False
        cases.append(
            {
                "name": name,
                "features": features,
                "no_default_features": no_default,
                "target": triple.group(1),
                "sdkconfig": sdk.group(1),
                "sdkconfig_full": True,  # every build.sh flow rides a .full config
                "provenance": "build.sh",
            }
        )
    return cases


def load_manifest_features() -> set[str]:
    with open(FW_MANIFEST, "rb") as f:
        manifest = tomllib.load(f)
    feats = set(manifest.get("features", {}).keys())

    def optional_deps(deps: dict) -> set[str]:
        return {name for name, spec in deps.items()
                if isinstance(spec, dict) and spec.get("optional")}

    opt: set[str] = optional_deps(manifest.get("dependencies", {}))
    for cfg in (manifest.get("target") or {}).values():
        opt |= optional_deps(cfg.get("dependencies", {}))

    # Cargo's dep: suppression rule (#402 review): an optional dependency
    # gets an implicit same-name feature ONLY while no feature references
    # it as dep:<name> — once referenced, the implicit feature is
    # suppressed and --features <dep-name> is INVALID. This manifest
    # dep:-references all of its optional deps, so the effective set is
    # the [features] keys; a future unreferenced optional dep would
    # correctly join the set.
    referenced = {t for lst in manifest.get("features", {}).values()
                  for t in lst if isinstance(t, str)}
    suppressed = {n.removeprefix("dep:") for n in referenced if n.startswith("dep:")}
    return feats | (opt - suppressed)


def sdkconfig_paths_ok(sdkconfig: str) -> bool:
    if any(part not in ("sdkconfig.defaults", "sdkconfig.defaults.esp32") for part in sdkconfig.split(";")):
        return all((FW_DIR / part).exists() for part in sdkconfig.split(";"))
    # defaults-family entries are optional per-chip overlays; require at
    # least the base file to exist
    return (FW_DIR / sdkconfig.split(";")[0]).exists()


def check() -> list[str]:
    """Full validation; returns a list of error strings (empty == healthy)."""
    errors: list[str] = []
    try:
        text = BUILD_SH.read_text()
        combos = parse_build_sh(text)
    except (OSError, ValueError) as e:
        return [f"build.sh parse failed: {e}"]

    if len(combos) < 10:
        errors.append(f"build.sh yielded only {len(combos)} cases — parser drift?")

    known_features = load_manifest_features()
    for c in combos + CI_ONLY_COMBOS:
        for feat in filter(None, c["features"].split(",")):
            if feat not in known_features:
                errors.append(f"{c['name']}: feature {feat!r} not in esp32-ccid [features]")
        if c["target"] not in ("xtensa-esp32-espidf", "riscv32imc-esp-espidf"):
            errors.append(f"{c['name']}: unexpected target {c['target']}")
        if not sdkconfig_paths_ok(c["sdkconfig"]):
            errors.append(f"{c['name']}: sdkconfig {c['sdkconfig']!r} missing under firmware/esp32-ccid/")
        # --no-default-features with an EMPTY feature list would compile an
        # empty backend-less firmware — that is the malformed shape. An
        # empty list WITH default features (m5atom) is the intended flow.
        if not c["features"] and c["no_default_features"]:
            errors.append(f"{c['name']}: --no-default-features with empty feature list — malformed entry")
    names = [c["name"] for c in combos + CI_ONLY_COMBOS]
    if len(names) != len(set(names)):
        errors.append(f"duplicate variant names: {sorted(names)}")

    # Host combos validate against the same feature vocabulary.
    for h in HOST_COMBOS:
        for feat in h["features"].split(","):
            if feat not in known_features:
                errors.append(f"{h['name']}: feature {feat!r} not in esp32-ccid [features]")

    # ccid-service boundary allowlist (mirrors the rule documented in its
    # Cargo.toml) — keeps the serving kernel board-blind at the dep edge.
    try:
        with open(REPO_ROOT / "crates" / "ccid-service" / "Cargo.toml", "rb") as f:
            service_deps = set(tomllib.load(f).get("dependencies", {}).keys())
        stray = service_deps - CCID_SERVICE_ALLOWED_DEPS
        if stray:
            errors.append(f"ccid-service grew deps outside the boundary allowlist: {sorted(stray)}")
    except OSError as e:
        errors.append(f"ccid-service manifest unreadable: {e}")

    errors.extend(selftest(quiet=True))
    return errors


SELFTEST_FIXTURE = '''#!/usr/bin/env bash
case "${BOARD}" in
    alpha)
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig.full"
        TRIPLE="riscv32imc-esp-espidf"
        PROFILE="debug"
        FEATURE_ARGS=(--no-default-features --features "wifi-ccid,backend-pn7160,board-nucula")
        ;;
    beta)
        # comment line inside a case
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig-xtensa.full"
        TRIPLE="xtensa-esp32-espidf"
        FEATURE_ARGS=(--no-default-features --features backend-mfrc522,board-m5stick)
        ;;
    gamma)
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig-xtensa.full"
        TRIPLE="xtensa-esp32-espidf"
        FEATURE_ARGS=()
        ;;
    *)
        exit 1
        ;;
esac
'''


def selftest(quiet: bool = False) -> list[str]:
    """Parser invariants against a synthetic fixture (no repo state needed)."""
    errors: list[str] = []
    try:
        cases = parse_build_sh(SELFTEST_FIXTURE)
    except ValueError as e:
        return [f"selftest parse failed: {e}"]
    expect = {
        "alpha": ("wifi-ccid,backend-pn7160,board-nucula", True, "riscv32imc-esp-espidf", "sdkconfig.full"),
        "beta": ("backend-mfrc522,board-m5stick", True, "xtensa-esp32-espidf", "sdkconfig-xtensa.full"),
        "gamma": ("", False, "xtensa-esp32-espidf", "sdkconfig-xtensa.full"),
    }
    got = {c["name"]: (c["features"], c["no_default_features"], c["target"], c["sdkconfig"]) for c in cases}
    if got != expect:
        errors.append(f"selftest: parser output drifted: {got!r}")
    # #394: a non-matching --filter must exit (vacuous-green guard); a
    # matching one keeps exactly the hits.
    try:
        select(cases, None, "no-such-combo")
        errors.append("selftest: non-matching --filter did not exit")
    except SystemExit:
        pass
    if [c["name"] for c in select(cases, None, "lph")] != ["alpha"]:
        errors.append("selftest: --filter substring match drifted")
    if not quiet:
        print("selftest: OK" if not errors else f"selftest: FAILED {errors}")
    return errors


def dims(combo: dict) -> str:
    """Human dimension summary (backend x board x transport x mode)."""
    f = set(filter(None, combo["features"].split(",")))
    # pn7160-* variant features (pn7160-ccid, pn7160-bringup, ...) imply
    # backend-pn7160 + board-nucula through the manifest's feature deps.
    if any(x.startswith("pn7160-") for x in f):
        f |= {"backend-pn7160", "board-nucula"}
    backend = next((x for x in f if x.startswith("backend-")), "default(mfrc522)")
    board = next((x for x in f if x.startswith("board-")), "default(m5atom)")
    transport = ("vpcd-tcp" if "wifi-vpcd" in f else
                 "wifi-tcp" if "wifi-ccid" in f else
                 "bt-spp" if "spp" in f else
                 "usb-cdc")
    mode = ("ble-console" if "ble" in f or "ble-diag" in f else
            "bringup" if "pn7160-bringup" in f else
            "vpcd" if "wifi-vpcd" in f else
            "lcd" if "lcd" in f else
            "ccid")
    return f"{backend.removeprefix('backend-')} x {board.removeprefix('board-')} x {transport} x {mode}"


def select(combos: list[dict], variant: str | None, filt: str | None) -> list[dict]:
    out = combos
    if variant:
        out = [c for c in out if c["name"] == variant]
        if not out:
            sys.exit(f"error: unknown variant {variant!r} (see --format table)")
    if filt:
        out = [c for c in out if filt in c["name"]]
        # #394: an empty filtered set must be LOUD (exit contract, line 32).
        # In the workflow a silent [] skips the whole matrix job — a typo'd
        # dispatch filter would produce a vacuous green run.
        if not out:
            sys.exit(f"error: --filter {filt!r} matched no variants (see --format table)")
    return out


def main() -> int:
    args = sys.argv[1:]
    fmt = "check"
    if args and args[0] == "selftest":
        return 1 if selftest() else 0
    if args and args[0] == "check":
        args = args[1:]
    elif args and args[0] == "--format":
        fmt = args[1]
        args = args[2:]
    variant = filt = None
    while args:
        if args[0] == "--variant":
            variant = args[1]
            args = args[2:]
        elif args[0] == "--filter":
            filt = args[1]
            args = args[2:]
        else:
            sys.exit(f"error: unknown arg {args[0]!r}")

    if fmt == "check":
        errs = check()
        for e in errs:
            print(f"::error::{e}" if not sys.stderr.isatty() else f"ERROR: {e}", file=sys.stderr)
        if errs:
            return 1
        combos = parse_build_sh(BUILD_SH.read_text()) + CI_ONLY_COMBOS
        print(f"union matrix OK: {len(combos)} esp combos, {len(HOST_COMBOS)} host combos, ccid-service boundary clean")
        return 0

    combos = parse_build_sh(BUILD_SH.read_text()) + CI_ONLY_COMBOS
    combos = select(combos, variant, filt)

    if fmt == "matrix":
        print(json.dumps(combos, separators=(",", ":")))
    elif fmt == "host-matrix":
        print(json.dumps(HOST_COMBOS, separators=(",", ":")))
    elif fmt == "table":
        print(f"{'variant':<22} {'target':<24} {'dims'}")
        for c in combos:
            print(f"{c['name']:<22} {c['target']:<24} {dims(c)}")
        for c in combos:
            print(f"  {c['name']:<22} {c['features'] or '(defaults)'}   [{c['provenance']}]")
    elif fmt == "commands":
        for c in combos:
            print(f"# --- {c['name']} ({dims(c)}) [{c['provenance']}]")
            print("cd firmware/esp32-ccid")
            if c["sdkconfig_full"]:
                print(f'export ESP_IDF_SDKCONFIG="$PWD/{c["sdkconfig"]}"')
                print('export ESP_IDF_GLOB_PARTCSV_BASE="$PWD"')
                print('export ESP_IDF_GLOB_PARTCSV_1="/partitions-ota.csv"')
            else:
                print(f'export ESP_IDF_SDKCONFIG_DEFAULTS="$PWD/{c["sdkconfig"]}"')
            if c["no_default_features"]:
                print(f'cargo +esp check --target {c["target"]} --no-default-features --features "{c["features"]}"')
            else:
                print(f'cargo +esp check --target {c["target"]}')
            print()
    else:
        sys.exit(f"error: unknown format {fmt!r}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
