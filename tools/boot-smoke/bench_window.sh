#!/usr/bin/env bash
# bench_window.sh — one serialized bench window for a boot-smoke leg.
#
# Wraps a payload script with ALL THREE bench-exclusion layers
# (docs/HIL-CI-PRACTICES.md §2):
#   1. labgrid RESERVATION (the queue: waits for the nucula-c3 place
#      instead of failing; interactive sessions lock the same places,
#      so humans and CI share one coordinator view),
#   2. the cross-project flock (non-labgrid tooling on this host),
#   3. the operator kill-switch marker (skip-with-notice: a green
#      pipeline that touched no hardware).
#
# Usage: bench_window.sh <payload.sh> [args...]
# Exit code: the payload's exit code, or 0 on operator-skip.
# The trap releases on ANY exit path; a SIGKILLed step self-heals —
# the flock dies with the process and the reservation times out
# unused (coordinator default), so no window is ever stranded.
set -uo pipefail

MARKER=/tmp/amperstrand-bench-ci.disabled
if [ -e "$MARKER" ]; then
    echo "SKIP: bench CI disabled by operator — $(cat "$MARKER" 2>/dev/null || echo 'no reason given')"
    echo "BENCH-CI-SKIPPED (operator) — no hardware touched"
    exit 0
fi

PAYLOAD="$1"; shift
[ -x "$PAYLOAD" ] || { echo "FAIL: payload not executable: $PAYLOAD" >&2; exit 2; }

PLACE="${BENCH_PLACE:-nucula-c3}"
RESERVE_WAIT_S="${BENCH_RESERVE_WAIT_S:-300}"

cleanup() {
    local rc=$?
    flock -u 9 2>/dev/null || true
    if [ -n "${LG_TOKEN:-}" ]; then
        labgrid-client -p + release >/dev/null 2>&1 || true
    fi
    exit "$rc"
}
trap cleanup EXIT INT TERM

# 1. reservation (queue, bounded wait)
if ! eval "$(timeout "$RESERVE_WAIT_S" labgrid-client reserve --wait --shell "name=${PLACE}")" \
   || [ -z "${LG_TOKEN:-}" ]; then
    echo "FAIL: no labgrid reservation for '${PLACE}' within ${RESERVE_WAIT_S}s (coordinator down, timed out, or place busy)" >&2
    exit 3
fi
echo "# labgrid reservation acquired: ${LG_TOKEN:-?} -> place '${PLACE}'"

# 2. place lock (ours exclusively while held)
if ! labgrid-client -p + lock; then
    echo "FAIL: could not lock reserved place '${PLACE}'" >&2
    exit 4
fi
echo "# labgrid place '${PLACE}' locked"

# 3. cross-project flock
exec 9>/tmp/amperstrand-bench.lock
if ! flock -w 30 9; then
    echo "FAIL: cross-project bench flock held >30s after reservation — operator lane active?" >&2
    exit 5
fi
echo "# bench flock held (fd 9); window is ours"

"$PAYLOAD" "$@"
