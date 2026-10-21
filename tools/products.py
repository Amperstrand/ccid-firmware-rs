#!/usr/bin/env python3
"""products.py — ONE source of truth resolver for M3 product presets (t_0492b7c6).

Products.toml is the manifest; this tool is its compiler and its gate:

  * `check` re-derives every preset LIVE from the build authorities —
    build.sh's case table (via union_matrix's parser — import, never
    copy) for esp32 rows, the crate manifests for the feature
    vocabulary — and fails LOUD on drift (unknown variant, unknown
    feature, remove-not-in-base, duplicate artifact). CI runs this on
    every union-matrix leg; a product row can never silently rot.
  * `resolve` maps product/board -> the concrete feature set; build.sh
    evals this output (the `product=<X> board=<Y>` entrypoint), so the
    bench flow and CI build the SAME resolved preset.
  * `--format matrix` generates the CI product-artifacts matrix (rows
    with `ci = true`) — the workflow never inlines product definitions.

Doctrine (inherited from union_matrix.py, t_f579fc05): the manifest is
the single place a product is defined; every consumer resolves through
this script. Products are DELTAS on the verified bench variants
(`variant` = the build.sh case), not copies of their feature lists —
copying would fork the truth.

Modes:
  check                    validate products.toml against build.sh + the
                           crate feature vocabularies + selftest; exit 1
  selftest                 resolver invariants on synthetic fixtures
  list                     one line per product: boards, artifacts, CI
  resolve --product P [--board B]
                           concrete resolution; --format shell (default;
                           PD_* assignments for build.sh to eval) or json
  --format matrix          GitHub matrix JSON for ci=true rows
  --format table           human table: product x board -> features
  --format commands        the local build entrypoint per row
  --product/--board        restrict table/commands output

Exit codes: 0 ok, 1 validation failure / unknown product or board.
"""

from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path

TOOLS_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS_DIR))
import union_matrix  # noqa: E402  (same-dir import; the build.sh authority)

REPO_ROOT = TOOLS_DIR.parent
PRODUCTS_TOML = REPO_ROOT / "products.toml"
BUILD_SH = REPO_ROOT / "firmware" / "esp32-ccid" / "build.sh"
FW_ESP = REPO_ROOT / "firmware" / "esp32-ccid"
FW_STM32 = REPO_ROOT / "firmware" / "ccid-firmware"

ESP_TRIPLES = ("xtensa-esp32-espidf", "riscv32imc-esp-espidf")


def load_products() -> dict:
    with open(PRODUCTS_TOML, "rb") as f:
        return tomllib.load(f)["products"]


# union_matrix's parser extracts triple/sdkconfig/features but NOT the
# profile — products must know it (release vs debug decides the artifact
# path and the CI cache key), so re-scan the same case chunks for it.
PROFILE_RE = re.compile(r'PROFILE="([^"]+)"')


def build_sh_cases() -> dict[str, dict]:
    """The bench authority, parsed live (union_matrix's parser — not a copy).

    Supplemented with the per-case PROFILE (release/debug), which
    union_matrix has no need for but product artifacts do: the profile
    decides the ELF path and the CI artifact cache key.
    """
    text = BUILD_SH.read_text()
    cases = {c["name"]: c for c in union_matrix.parse_build_sh(text)}
    # Re-scan the same case BLOCK (not the whole file) for PROFILE lines,
    # chunked exactly like union_matrix.parse_build_sh does.
    start = text.index('case "${BOARD}" in')
    end = text.index("\nesac", start)
    block = text[start:end]
    matches = list(union_matrix.CASE_RE.finditer(block))
    for i, m in enumerate(matches):
        name = m.group(1)
        if name not in cases:
            continue
        chunk_end = matches[i + 1].start() if i + 1 < len(matches) else len(block)
        prof = PROFILE_RE.search(block[m.end():chunk_end])
        if prof:
            cases[name]["profile"] = prof.group(1)
    return cases


# ---------------------------------------------------------------- resolve

def resolve_row(products: dict, product: str, board: str | None,
                cases: dict[str, dict] | None = None) -> dict:
    """product/board -> concrete {features, no_default_features, ...}.

    Raises ProductError (message for humans) on any mismatch — the
    resolver is the gate: unknown product, unknown board, unknown base
    variant, remove-not-in-base, add-already-present.
    """
    cases = cases if cases is not None else build_sh_cases()
    if product not in products:
        known = ", ".join(sorted(products))
        raise ProductError(f"unknown product {product!r} (products.toml has: {known})")
    boards = products[product].get("boards", {})
    if not boards:
        raise ProductError(f"product {product!r} declares no boards")
    if board is None:
        if len(boards) == 1:
            board = next(iter(boards))
        else:
            known = ", ".join(sorted(boards))
            raise ProductError(f"product {product!r} has multiple boards — pass one of: {known}")
    if board not in boards:
        known = ", ".join(sorted(boards))
        raise ProductError(f"unknown board {board!r} for product {product!r} (has: {known})")
    row = boards[board]

    if "variant" in row:  # esp32 row: delta on the build.sh case
        variant = row["variant"]
        if variant not in cases:
            raise ProductError(
                f"{product}/{board}: base variant {variant!r} not in build.sh — "
                "the preset must ride a verified bench case"
            )
        base = cases[variant]
        no_default = base["no_default_features"]
        if not no_default and row.get("remove-features", []):
            raise ProductError(
                f"{product}/{board}: remove-features is invalid on default-features "
                f"base variant {variant!r} (defaults are implicit — pick an "
                "explicit-features variant as the base)"
            )
        if no_default:
            feats = [f for f in filter(None, base["features"].split(","))]
            for rm in row.get("remove-features", []):
                if rm not in feats:
                    raise ProductError(
                        f"{product}/{board}: remove-features lists {rm!r} but base "
                        f"variant {variant!r} does not enable it (drift?)"
                    )
                feats.remove(rm)
            for add in row.get("add-features", []):
                if add in feats:
                    raise ProductError(
                        f"{product}/{board}: add-features lists {add!r} already "
                        f"enabled by base variant {variant!r} (drift?)"
                    )
                feats.append(add)
            final = ",".join(feats)
        else:
            # defaults ride along; cargo composes `--features X` on top
            final = ",".join(row.get("add-features", []))
        return {
            "product": product,
            "board": board,
            "dispatcher": "build.sh",
            "base_variant": variant,
            "features": final,
            "no_default_features": no_default,
            "target": base["target"],
            "sdkconfig": base["sdkconfig"],
            "sdkconfig_full": base["sdkconfig_full"],
            "profile": base.get("profile", "debug"),
            "artifact": row["artifact"],
            "ci": bool(row.get("ci", False)),
        }

    # cargo row (stm32-f469 today): direct crate/target/features.
    crate = products[product].get("crate")
    if not crate:
        raise ProductError(f"{product}/{board}: row has neither `variant` nor product-level `crate`")
    no_default = bool(row.get("no-default-features", False))
    feats = row.get("features", [])
    if feats and not no_default:
        # defaults PLUS explicit features is a valid cargo shape, but none of
        # today's products use it — refuse so the manifest stays unambiguous.
        raise ProductError(
            f"{product}/{board}: features with no-default-features=false is "
            "unused/ambiguous — set no-default-features=true or drop features"
        )
    return {
        "product": product,
        "board": board,
        "dispatcher": "cargo",
        "crate": crate,
        "features": ",".join(feats),
        "no_default_features": no_default,
        "target": products[product]["target"],
        "profile": products[product].get("profile", "release"),
        "artifact": row["artifact"],
        "ci": bool(row.get("ci", False)),
    }


class ProductError(Exception):
    pass


# ----------------------------------------------------------------- check

def _esp_feature_vocab() -> set[str]:
    return union_matrix.load_manifest_features()


def _stm32_feature_vocab() -> set[str]:
    with open(FW_STM32 / "Cargo.toml", "rb") as f:
        return set(tomllib.load(f).get("features", {}).keys())


def check() -> list[str]:
    """Full validation; returns error strings (empty == healthy)."""
    errors: list[str] = []
    try:
        products = load_products()
    except (OSError, tomllib.TOMLDecodeError, KeyError) as e:
        return [f"products.toml unreadable: {e}"]
    try:
        cases = build_sh_cases()
    except (OSError, ValueError) as e:
        return [f"build.sh parse failed: {e}"]

    esp_feats = _esp_feature_vocab()
    stm32_feats = _stm32_feature_vocab()
    artifacts: dict[str, str] = {}

    for pname, pdef in sorted(products.items()):
        boards = pdef.get("boards", {})
        if not boards:
            errors.append(f"{pname}: no boards declared")
            continue
        for bname, row in sorted(boards.items()):
            tag = f"{pname}/{bname}"
            art = row.get("artifact")
            if not art:
                errors.append(f"{tag}: missing artifact name")
            elif art in artifacts:
                errors.append(f"{tag}: artifact {art!r} already used by {artifacts[art]}")
            else:
                artifacts[art] = tag
            try:
                r = resolve_row(products, pname, bname, cases=cases)
            except ProductError as e:
                errors.append(str(e))
                continue
            if r["dispatcher"] == "build.sh":
                for feat in filter(None, r["features"].split(",")):
                    if feat not in esp_feats:
                        errors.append(f"{tag}: feature {feat!r} not in esp32-ccid [features]")
                if r["target"] not in ESP_TRIPLES:
                    errors.append(f"{tag}: base variant target {r['target']!r} is not an esp triple")
            else:
                for feat in filter(None, r["features"].split(",")):
                    if feat not in stm32_feats:
                        errors.append(f"{tag}: feature {feat!r} not in ccid-firmware [features]")

    errors.extend(selftest(quiet=True))
    return errors


# -------------------------------------------------------------- selftest

def selftest(quiet: bool = False) -> list[str]:
    """Resolver invariants on synthetic fixtures (no repo state touched)."""
    errors: list[str] = []
    cases = {
        "m5stick": {"features": "backend-mfrc522,board-m5stick", "no_default_features": True,
                    "target": "xtensa-esp32-espidf",
                    "sdkconfig": "sdkconfig-xtensa.full", "sdkconfig_full": True, "profile": "release"},
        "m5atom": {"features": "", "no_default_features": False,
                   "target": "xtensa-esp32-espidf",
                   "sdkconfig": "sdkconfig-xtensa.full", "sdkconfig_full": True, "profile": "release"},
    }
    products = {
        "alpha": {"boards": {
            "one": {"variant": "m5stick", "add-features": ["provisioner"],
                    "remove-features": [], "artifact": "alpha-one", "ci": True},
        }},
        "beta": {"boards": {
            "two": {"variant": "m5atom", "add-features": ["provisioner"],
                    "remove-features": [], "artifact": "beta-two", "ci": True},
        }},
        "gamma": {"boards": {  # remove + add arithmetic
            "three": {"variant": "m5stick", "add-features": ["ble"],
                      "remove-features": ["board-m5stick"], "artifact": "gamma-three"},
        }},
    }

    def expect_error(fn, needle, what):
        try:
            fn()
        except ProductError as e:
            if needle not in str(e):
                errors.append(f"selftest {what}: error {e!r} lacks {needle!r}")
            return
        errors.append(f"selftest {what}: expected ProductError, got success")

    r = resolve_row(products, "alpha", "one", cases=cases)
    if r["features"] != "backend-mfrc522,board-m5stick,provisioner":
        errors.append(f"selftest add: got {r['features']!r}")
    if not r["no_default_features"]:
        errors.append("selftest add: no_default_features should be true")
    r = resolve_row(products, "beta", "two", cases=cases)
    if r["features"] != "provisioner" or r["no_default_features"]:
        errors.append(f"selftest default+add: got {r['features']!r} ndf={r['no_default_features']}")
    r = resolve_row(products, "gamma", "three", cases=cases)
    if r["features"] != "backend-mfrc522,ble":
        errors.append(f"selftest remove+add: got {r['features']!r}")

    expect_error(lambda: resolve_row(products, "nope", "one", cases=cases), "unknown product", "unknown product")
    expect_error(lambda: resolve_row(products, "alpha", "nope", cases=cases), "unknown board", "unknown board")
    multi = {"multi": {"boards": {
        "one": dict(products["alpha"]["boards"]["one"], artifact="alpha-one"),
        "x": dict(products["alpha"]["boards"]["one"], artifact="alpha-x"),
    }}}
    expect_error(lambda: resolve_row(multi, "multi", None, cases=cases), "multiple boards", "board required")
    bad_variant = {"bad": {"boards": {"b": {"variant": "ghost", "add-features": [], "artifact": "z"}}}}
    expect_error(lambda: resolve_row(bad_variant, "bad", "b", cases=cases), "not in build.sh", "unknown variant")
    bad_remove = {"bad2": {"boards": {"b": {"variant": "m5stick", "remove-features": ["ble"], "artifact": "z"}}}}
    expect_error(lambda: resolve_row(bad_remove, "bad2", "b", cases=cases), "does not enable", "remove missing")
    bad_add = {"bad3": {"boards": {"b": {"variant": "m5stick", "add-features": ["board-m5stick"], "artifact": "z"}}}}
    expect_error(lambda: resolve_row(bad_add, "bad3", "b", cases=cases), "already enabled", "dup add")
    bad_defrm = {"bad4": {"boards": {"b": {"variant": "m5atom", "remove-features": ["ble"], "artifact": "z"}}}}
    expect_error(lambda: resolve_row(bad_defrm, "bad4", "b", cases=cases), "default-features", "remove on defaults")

    if not errors and not quiet:
        print("selftest: ok")
    return errors


# ------------------------------------------------------------- formatters

def fmt_list(products: dict) -> str:
    out = []
    for pname, pdef in sorted(products.items()):
        pending = pdef.get("pending", [])
        pend = f"  [pending: {'; '.join(pending)}]" if pending else ""
        out.append(f"{pname}{pend}")
        for bname, row in sorted(pdef.get("boards", {}).items()):
            if "variant" in row:
                base = f"variant={row['variant']}"
                delta = []
                if row.get("add-features"):
                    delta.append(f"+{','.join(row['add-features'])}")
                if row.get("remove-features"):
                    delta.append(f"-{','.join(row['remove-features'])}")
                comp = base + (f" {' '.join(delta)}" if delta else "")
            else:
                comp = f"crate={pdef.get('crate')} target={pdef.get('target')}"
            ci = "ci-artifact" if row.get("ci") else "-"
            out.append(f"  {bname:<16} {comp:<52} artifact={row['artifact']} [{ci}]")
    return "\n".join(out)


def fmt_table(products: dict, cases: dict[str, dict],
              product: str | None, board: str | None) -> str:
    lines = [f"{'product/board':<28} {'target':<24} {'profile':<8} features"]
    lines.append("-" * 100)
    for pname in sorted(products):
        if product and pname != product:
            continue
        for bname in sorted(products[pname].get("boards", {})):
            if board and bname != board:
                continue
            try:
                r = resolve_row(products, pname, bname, cases=cases)
            except ProductError as e:
                lines.append(f"{pname}/{bname}: ERROR {e}")
                continue
            feats = r["features"] or "(crate defaults)"
            if not r["no_default_features"] and r["features"]:
                feats = f"(defaults)+{feats}"
            lines.append(
                f"{pname}/{bname:<27} {r['target']:<24} {r['profile']:<8} {feats}"
            )
    return "\n".join(lines)


def fmt_commands(products: dict, cases: dict[str, dict],
                 product: str | None, board: str | None) -> str:
    lines = []
    for pname in sorted(products):
        if product and pname != product:
            continue
        for bname in sorted(products[pname].get("boards", {})):
            if board and bname != board:
                continue
            r = resolve_row(products, pname, bname, cases=cases)
            if r["dispatcher"] == "build.sh":
                lines.append(f"# {r['artifact']}")
                lines.append(f"cd firmware/esp32-ccid && ./build.sh product={pname} board={bname}")
            else:
                feats = ""
                if r["no_default_features"]:
                    feats = f" --no-default-features --features {r['features']}"
                elif r["features"]:
                    feats = f" --features {r['features']}"
                prof = "" if r["profile"] == "debug" else f" --{r['profile']}"
                lines.append(f"# {r['artifact']}")
                lines.append(f"cargo build{prof} -p {r['crate']} --target {r['target']}{feats}")
            lines.append("")
    return "\n".join(lines)


def fmt_matrix(products: dict, cases: dict[str, dict]) -> str:
    """GitHub matrix include JSON for ci=true rows (the artifact legs)."""
    rows = []
    for pname in sorted(products):
        for bname in sorted(products[pname].get("boards", {})):
            r = resolve_row(products, pname, bname, cases=cases)
            if not r["ci"] or r["dispatcher"] != "build.sh":
                continue
            rows.append({
                "name": r["artifact"],
                "product": pname,
                "board": bname,
                "features": r["features"],
                "no_default_features": r["no_default_features"],
                "target": r["target"],
                "sdkconfig": r["sdkconfig"],
                "sdkconfig_full": r["sdkconfig_full"],
                "profile": r["profile"],
            })
    return json.dumps(rows, separators=(",", ":"))


def fmt_resolve_shell(r: dict) -> str:
    """PD_* assignments for build.sh to eval (quoted; feature names are
    [a-z0-9-] so quoting is belt-and-braces)."""
    lines = [
        f"PD_PRODUCT='{r['product']}'",
        f"PD_BOARD='{r['board']}'",
        f"PD_DISPATCHER='{r['dispatcher']}'",
        f"PD_ARTIFACT='{r['artifact']}'",
    ]
    if r["dispatcher"] == "build.sh":
        lines += [
            f"PD_BASE_VARIANT='{r['base_variant']}'",
            f"PD_FEATURES='{r['features']}'",
            f"PD_NO_DEFAULT_FEATURES='{1 if r['no_default_features'] else 0}'",
        ]
    else:
        feats = r["features"]
        ndf = "--no-default-features " if r["no_default_features"] else ""
        fargs = f" --features {feats}" if feats else ""
        prof = "" if r["profile"] == "debug" else f" --{r['profile']}"
        lines.append(
            f"PD_HINT=\"cargo build{prof} -p {r['crate']} --target {r['target']} {ndf}{fargs}\""
        )
    return "\n".join(lines)


# ------------------------------------------------------------------ main

def main(argv: list[str]) -> int:
    args = argv[1:]
    fmt = None
    product = None
    board = None
    out_fmt = "shell"

    i = 0
    while i < len(args):
        a = args[i]
        if a == "check":
            fmt = "check"
        elif a == "selftest":
            fmt = "selftest"
        elif a == "list":
            fmt = "list"
        elif a == "resolve":
            fmt = "resolve"
        elif a == "--format":
            i += 1
            if i >= len(args):
                print("error: --format requires a value", file=sys.stderr)
                return 1
            out_fmt = args[i]
            if fmt is None:
                fmt = out_fmt
        elif a == "--product":
            i += 1
            product = args[i]
        elif a == "--board":
            i += 1
            board = args[i]
        else:
            print(f"error: unknown argument {a!r}", file=sys.stderr)
            return 1
        i += 1

    if fmt is None:
        print(__doc__)
        return 1

    if fmt == "selftest":
        errs = selftest()
        if errs:
            print("\n".join(errs), file=sys.stderr)
            return 1
        print("selftest: ok")
        return 0

    if fmt == "check":
        errs = check()
        if errs:
            print("products.toml check FAILED:", file=sys.stderr)
            print("\n".join(f"  - {e}" for e in errs), file=sys.stderr)
            return 1
        print("products.toml check: ok")
        return 0

    try:
        products = load_products()
    except (OSError, tomllib.TOMLDecodeError, KeyError) as e:
        print(f"error: products.toml unreadable: {e}", file=sys.stderr)
        return 1

    if fmt == "list":
        print(fmt_list(products))
        return 0

    if fmt == "resolve":
        if product is None:
            print("error: resolve requires --product <name> (board optional when "
                  "the product has exactly one board)", file=sys.stderr)
            return 1
        try:
            r = resolve_row(products, product, board)
        except ProductError as e:
            print(f"error: {e}", file=sys.stderr)
            return 1
        if out_fmt == "json":
            print(json.dumps(r, indent=2))
        else:
            print(fmt_resolve_shell(r))
        return 0

    try:
        cases = build_sh_cases()
    except (OSError, ValueError) as e:
        print(f"error: build.sh parse failed: {e}", file=sys.stderr)
        return 1

    if fmt == "matrix":
        print(fmt_matrix(products, cases))
        return 0
    if fmt == "table":
        print(fmt_table(products, cases, product, board))
        return 0
    if fmt == "commands":
        print(fmt_commands(products, cases, product, board))
        return 0
    if fmt == "shell":  # --format shell without resolve: nothing to shell out
        print("error: --format shell only pairs with `resolve`", file=sys.stderr)
        return 1
    print(f"error: unknown format {fmt!r}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
