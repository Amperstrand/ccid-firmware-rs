#!/usr/bin/env bash
# Forced-red fixture for the boot-smoke gate's demonstration run (#466
# DONE MEANS (b)): create a PR branch that reintroduces the exact #278
# failure signature — the i2c: CONFLICT abort at boot.
#
# 2026-10-31 refresh: the original method (git-revert of the #292 merge)
# stopped applying cleanly once boards-as-data reworked the area — the
# #292 fix was ONE sdkconfig knob, so the fixture flips it directly:
#   CONFIG_I2C_SKIP_LEGACY_CONFLICT_CHECK=y  ->  not set
# in firmware/esp32-ccid/sdkconfig.full (the bench build's config).
# The demonstration run must then show boot_check's CONFLICT veto fire
# (gate RED) and the restore step flash known-good back with the
# uptime-monotonic probe (DONE MEANS (c)).
#
# Usage: tools/boot-smoke/forced-red-fixture.sh [base-ref]   (default origin/main)
# Pure git + one config line — no hardware, no push.
set -euo pipefail

BASE="${1:-origin/main}"
BRANCH="boot-smoke/forced-red-292"
KNOB="CONFIG_I2C_SKIP_LEGACY_CONFLICT_CHECK"
CFG="firmware/esp32-ccid/sdkconfig.full"

git fetch origin main --quiet
git switch -C "${BRANCH}" "${BASE}"

if ! grep -q "^${KNOB}=y$" "${CFG}"; then
    echo "FATAL: ${KNOB}=y not found in ${CFG} — the #292 fix moved again; refresh this fixture (never hand-force a safety fixture)" >&2
    exit 2
fi
sed -i "s/^${KNOB}=y\$/# ${KNOB} is not set/" "${CFG}"
git add "${CFG}"
git -c user.name="Amperstrand" -c user.email="141745238+Amperstrand@users.noreply.github.com" \
    commit -q -m "fixture(boot-smoke): forced-red — reintroduce the #278 i2c-CONFLICT boot abort

Flip CONFIG_I2C_SKIP_LEGACY_CONFLICT_CHECK off in sdkconfig.full: the
exact #292 fix inverted (refreshed fixture — the merge no longer
reverts cleanly post-boards-as-data, but the fix was this one knob).
The demonstration run MUST show boot_check's CONFLICT veto fire (gate
RED) and the restore step flash known-good back with the
uptime-monotonic probe. DO NOT MERGE — demonstration branch only."

echo ""
echo "Forced-red branch '${BRANCH}' ready at $(git rev-parse --short HEAD):"
echo "  1. git push -u origin ${BRANCH}"
echo "  2. open a PR, add labels 'boot-smoke' + 'boot-smoke-bench'"
echo "  3. EXPECT: bench leg RED with the i2c CONFLICT veto + known-good restore"
