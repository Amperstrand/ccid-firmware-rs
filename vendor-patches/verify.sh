#!/usr/bin/env bash
# vendor-patches/verify.sh — mechanical half of the #271 patch lifecycles.
#
# Legs (no args = all):
#   synopsys  clone upstream stm32-rs/synopsys-usb-otg, checkout the
#             recorded base, git apply --check + git am the series, and
#             compare the resulting tree hash with the recorded fork tree.
#   snapshot  sha256sum -c the sole-provider BSP vendor tree against
#             stm32f469i-disc/SHA256SUMS.
#
# Env overrides (for offline/fast re-runs):
#   SYNOPSYS_UPSTREAM_URL  git URL to clone instead of stm32-rs GitHub
#                          (must contain the recorded base commit).
#   TMPDIR_BASE            parent for scratch dirs (default: mktemp -d).
#
# Exit 0 only if every requested leg passes.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

SYNOPSYS_BASE="f5ed46479e742e0d9441ae321a2a2f2a15e397d0"      # = tag v0.5.0
SYNOPSYS_FORK_TREE="6f5ee395420ebe2e2cae9d4006e0132680b6ef57" # Amperstrand fork master @ 764bc04
SYNOPSYS_URL="${SYNOPSYS_UPSTREAM_URL:-https://github.com/stm32-rs/synopsys-usb-otg}"

rc_overall=0

leg_synopsys() {
    local tmp
    tmp="$(mktemp -d "${TMPDIR_BASE:-/tmp}/vp-synopsys.XXXXXX")" || return 1
    local series=("$HERE/synopsys-usb-otg/series"/000[12]-*.patch)
    if [ ! -f "${series[0]}" ]; then
        echo "synopsys: FAIL — no series patches under synopsys-usb-otg/series/"
        return 1
    fi
    git clone --quiet "$SYNOPSYS_URL" "$tmp/up" || { echo "synopsys: FAIL — clone $SYNOPSYS_URL"; rm -rf "$tmp"; return 1; }
    git -C "$tmp/up" checkout --quiet "$SYNOPSYS_BASE" || { echo "synopsys: FAIL — base $SYNOPSYS_BASE not reachable (upstream moved? rebase the series)"; rm -rf "$tmp"; return 1; }
    git -C "$tmp/up" apply --check "${series[@]}" || { echo "synopsys: FAIL — git apply --check (base $SYNOPSYS_BASE)"; rm -rf "$tmp"; return 1; }
    echo "synopsys: git apply --check OK (${#series[@]} patches onto $SYNOPSYS_BASE)"
    git -C "$tmp/up" -c user.name=verify -c user.email=verify@localhost \
        am --quiet "${series[@]}" || { echo "synopsys: FAIL — git am"; rm -rf "$tmp"; return 1; }
    local tree
    tree="$(git -C "$tmp/up" rev-parse 'HEAD^{tree}')"
    rm -rf "$tmp"
    if [ "$tree" != "$SYNOPSYS_FORK_TREE" ]; then
        echo "synopsys: FAIL — series result tree $tree != recorded fork tree $SYNOPSYS_FORK_TREE (series drifted from the fork delta)"
        return 1
    fi
    echo "synopsys: OK — applies onto $SYNOPSYS_BASE, tree == fork master ($SYNOPSYS_FORK_TREE)"
}

leg_snapshot() {
    local sums="$HERE/stm32f469i-disc/SHA256SUMS"
    if [ ! -f "$sums" ]; then
        echo "snapshot: FAIL — missing $sums"
        return 1
    fi
    (cd "$ROOT" && sha256sum --quiet --check "$sums") || { echo "snapshot: FAIL — vendor/stm32f469i-disc drifted from SHA256SUMS (sole-provider tree edited; update sums + say why, in the same change)"; return 1; }
    echo "snapshot: OK — $(wc -l < "$sums") files match SHA256SUMS"
}

run_leg() {
    local leg="$1" rc
    "leg_$leg"; rc=$?
    [ $rc -ne 0 ] && rc_overall=$rc
    return $rc
}

legs=("${@:-}")
if [ ${#legs[@]} -eq 0 ] || [ -z "${legs[0]}" ]; then legs=(synopsys snapshot); fi
for l in "${legs[@]}"; do
    case "$l" in
        synopsys|snapshot) run_leg "$l" ;;
        *) echo "unknown leg: $l (use: synopsys | snapshot)"; rc_overall=2 ;;
    esac
done
exit $rc_overall
