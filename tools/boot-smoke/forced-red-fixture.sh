#!/usr/bin/env bash
# Forced-red fixture for the boot-smoke gate's FIRST LIVE RUN (card
# t_ae840473 DONE MEANS (b)): create a PR branch that reintroduces the
# exact #278 failure signature — the i2c: CONFLICT abort at boot — by
# reverting the #292 fix ("skip IDF's legacy-i2c coexistence abort").
#
# The demonstration run then must show: boot_check's CONFLICT veto fires
# (gate RED), and the restore step flashes known-good back with an
# uptime-monotonic probe (card DONE MEANS (c)).
#
# Usage: tools/boot-smoke/forced-red-fixture.sh [base-ref]   (default origin/main)
# Pure git — no hardware, no push unless --push is given.
set -euo pipefail

BASE="${1:-origin/main}"
PUSH="${PUSH:-0}"
BRANCH="boot-smoke/forced-red-292"

git fetch origin main --quiet
MERGE="$(git log --merges --grep='#292' -1 --format='%H' "${BASE}")"
if [ -z "${MERGE}" ]; then
    echo "FATAL: cannot locate the #292 merge commit on ${BASE}" >&2
    exit 2
fi
echo "#292 merge commit: ${MERGE} ($(git log -1 --format=%s "${MERGE}"))"

git switch -C "${BRANCH}" "${BASE}"
# Revert the merge (mainline parent 1). Conflicts here would mean the
# fix has since been reworked — abort loudly, never hand-resolve a
# safety fixture.
if ! git revert --no-edit -m 1 "${MERGE}"; then
    git revert --abort 2>/dev/null || true
    echo "FATAL: revert did not apply cleanly — the fixture needs a refresh against current main" >&2
    exit 3
fi

echo ""
echo "Forced-red branch '${BRANCH}' ready at $(git rev-parse --short HEAD):"
echo "  1. git push -u origin ${BRANCH}"
echo "  2. open a PR, add the 'boot-smoke' label (bench legs stay skipped"
echo "     until the owner enables them — see docs/BOOT-SMOKE-GATE.md)"
echo "  3. EXPECTED when live: RED with 'i2c: CONFLICT' in the boot_check"
echo "     capture, followed by the known-good restore + uptime proof."
[ "${PUSH}" = "1" ] && git push -u origin "${BRANCH}"
exit 0
