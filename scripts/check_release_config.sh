#!/usr/bin/env bash
# Release configuration smoke test.
#
# Catches, before a tag is pushed or a manual release dispatched:
#   1. release.yml / Dockerfile / verify-reproducibility.sh referencing cargo
#      features that do not exist in firmware/ccid-firmware/Cargo.toml
#      (regression: the 2026-10 release matrix used profile-cherry-st2100 /
#      profile-gemalto-plain / profile-gemalto-pinpad — none ever existed,
#      every release build failed at `cargo build --features ...`)
#   2. Dockerfile COPY of files missing from the repo (regression:
#      rust-toolchain.toml was removed but still COPYed — docker build
#      failed at layer assembly)
#   3. manifest default dropping the MCU feature (issue #25: the stm32-lint
#      clippy pass builds default features; without stm32f469 it breaks)
#
# Static analysis only — no docker, no cargo, fast enough for every CI run.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"
MANIFEST="$ROOT/firmware/ccid-firmware/Cargo.toml"
fail=0

err() { echo "release-config: FAIL: $*" >&2; fail=1; }
ok() { echo "release-config: OK: $*"; }

[ -f "$MANIFEST" ] || { echo "release-config: FAIL: missing $MANIFEST" >&2; exit 1; }

# --- 1. collect valid feature names from the firmware manifest ----------
# Extract the [features] table of the *firmware package* manifest (the root
# workspace manifest has no [features]).
features_txt=$(awk '/^\[features\]/{f=1;next} /^\[/{f=0} f' "$MANIFEST")
[ -n "$features_txt" ] || { echo "release-config: FAIL: no [features] table in $MANIFEST" >&2; exit 1; }
valid=$(echo "$features_txt" | grep -vE '^[[:space:]]*#' | sed -E 's/=.*//' | tr -d ' ' | grep -v '^$' | grep -v '^default$')
ok "manifest features: $(echo "$valid" | tr '\n' ' ')"

is_valid_feature() { echo "$valid" | grep -qxF "$1"; }

check_feature_list() { # <label> <comma-list or "default" or "">
    local label="$1" list="$2"
    [ -z "$list" ] && { ok "$label: (default features)"; return; }
    [ "$list" = "default" ] && { ok "$label: default"; return; }
    IFS=',' read -ra toks <<< "$list"
    for tok in "${toks[@]}"; do
        tok="$(echo "$tok" | tr -d ' "')"
        [ -z "$tok" ] && continue
        if is_valid_feature "$tok"; then
            ok "$label: feature '$tok'"
        else
            err "$label references unknown feature '$tok' (not in $MANIFEST)"
        fi
    done
}

# --- 2. release.yml references ------------------------------------------
WF="$ROOT/.github/workflows/release.yml"
[ -f "$WF" ] || { echo "release-config: FAIL: missing $WF" >&2; exit 1; }
# matrix entries:  features: ""  /  - features: "a,b"
matrix_count=0
while IFS= read -r line; do
    list=$(echo "$line" | sed -E 's/^[[:space:]]*-?[[:space:]]*features:[[:space:]]*//')
    matrix_count=$((matrix_count + 1))
    check_feature_list "release.yml matrix" "$list"
done < <(grep -E '^[[:space:]]*-?[[:space:]]*features:' "$WF")
[ "$matrix_count" -ge 1 ] || err "release.yml: no matrix 'features:' entries found — extractor out of sync with the workflow"
# literal --build-arg PROFILE= or --features= usages (should not exist, but
# guard against regressions reintroducing them)
while IFS= read -r m; do
    err "release.yml hardcodes '$m' — matrix features must drive the build"
done < <(grep -Eo 'PROFILE=profile-[a-z0-9-]+' "$WF" || true)

# --- 3. Dockerfile references --------------------------------------------
DF="$ROOT/Dockerfile"
[ -f "$DF" ] || { echo "release-config: FAIL: missing $DF" >&2; exit 1; }
df_profile=$(sed -nE 's/^ARG PROFILE=([A-Za-z0-9_,.-]+).*/\1/p' "$DF" | head -1)
check_feature_list "Dockerfile ARG PROFILE" "${df_profile:-default}"

# every COPY source must exist in the repo
while read -r src _dst; do
    case "$src" in
        --from*) continue ;;     # multi-stage copies
        http*|https*) continue ;;
    esac
    [ -e "$ROOT/$src" ] && ok "Dockerfile COPY $src exists" \
        || err "Dockerfile COPY source '$src' does not exist (regression class: rust-toolchain.toml)"
done < <(sed -nE 's/^COPY ([^ ]+) .*/\1/p' "$DF")

# --- 4. issue #25 guard: default keeps exactly one MCU + one profile -----
def_line=$(echo "$features_txt" | grep -E '^default' || true)
if [ -z "$def_line" ]; then
    err "manifest has no 'default' feature (issue #25)"
else
    def_feats=$(echo "$def_line" | sed -E 's/^[^=]*=//; s/[]["]//g' | tr ',' '\n' | sed 's/^[[:space:]]*//' | grep -v '^$')
    mcu=$(echo "$def_feats" | grep -cE '^stm32f(469|746)$' || true)
    prof=$(echo "$def_feats" | grep -cE '^profile-' || true)
    [ "$mcu" -eq 1 ] || err "default must include exactly one MCU feature, got $mcu: $def_line"
    [ "$prof" -eq 1 ] || err "default must include exactly one profile feature, got $prof: $def_line"
    [ "$mcu" -eq 1 ] && [ "$prof" -eq 1 ] && ok "default features sane ($(echo "$def_feats" | tr '\n' ' '))"
fi

# --- 5. scripts/verify-reproducibility.sh default profile ---------------
VR="$ROOT/scripts/verify-reproducibility.sh"
if [ -f "$VR" ]; then
    vr_def=$(sed -nE 's/^PROFILE="\$\{1:-(profile-[a-z0-9-]+)\}".*/\1/p' "$VR" | head -1)
    if [ -n "$vr_def" ]; then
        if is_valid_feature "$vr_def"; then ok "verify-reproducibility.sh default '$vr_def'"; else
            err "verify-reproducibility.sh defaults to unknown feature '$vr_def'"
        fi
    fi
fi

if [ "$fail" -eq 0 ]; then
    echo "release-config: all checks passed"
else
    echo "release-config: FAILURES detected — fix before tagging" >&2
fi
exit "$fail"
