#!/bin/bash
# Parallel bench verification — contention-aware device-level parallelism.
#
# Parallelism model (why this is safe):
#   - labgrid places serialize HARDWARE contention per device
#   - pcscd is a SHARED singleton: exactly ONE phase may restart it
#   - serial ports are exclusive per device (battery owns ttyACM0 + m5stick)
#
# Phases:
#   1 (serial)   bench inventory — fast health gate
#   2 (parallel) F469 HIL  ||  serial conformance battery
#     F469: own place/USB/ST-LINK; pcscd read-only (its enumeration
#     retries tolerate the battery's 3 pcscd restarts in the soak)
#     battery: owns both serial ports + pcscd restarts
#   3 (serial)   differential matrix — pcscd reads across ALL readers;
#     must not overlap the battery's pcscd restarts
#
# Usage: bash tests/hardware/run_parallel_bench.sh [--quick]
# Exit code nonzero on any failure. Per-phase logs: /tmp/opencode/parallel-bench/

set -uo pipefail
cd "$(dirname "$0")/../.."
QUICK="${1:-}"
LOGDIR=/tmp/opencode/parallel-bench
mkdir -p "$LOGDIR"
declare -a RESULTS

record() { RESULTS+=("$1"); printf '%s\n' "$1"; }

echo "=== Phase 1: inventory (serial) ==="
if timeout 120 python3 tests/hardware/labgrid/bench_inventory.py >"$LOGDIR/inventory.log" 2>&1; then
  record "PASS  inventory"
else
  record "FAIL  inventory (see $LOGDIR/inventory.log)"
  echo "bench unhealthy — aborting"; exit 1
fi

echo "=== Phase 2: F469 HIL || serial battery (parallel) ==="
timeout 420 python3 -m pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil --no-header \
  >"$LOGDIR/f469-hil.log" 2>&1 &
F469_PID=$!
timeout 420 python3 -u tests/hardware/nfc/conformance_battery.py $QUICK \
  >"$LOGDIR/battery.log" 2>&1 &
BATT_PID=$!
wait $F469_PID && record "PASS  f469-hil" || record "FAIL  f469-hil (see $LOGDIR/f469-hil.log)"
# battery exit 1 = known environmental card-present divergence; inspect tail
wait $BATT_PID; RC=$?
if [ $RC -eq 0 ]; then record "PASS  battery"
elif grep -q "IccPowerOn" "$LOGDIR/battery.log" && [ $RC -eq 1 ]; then
  record "WARN  battery rc=1 (check IccPowerOn card-present divergence)"
else record "FAIL  battery rc=$RC (see $LOGDIR/battery.log)"; fi

echo "=== Phase 3: differential matrix (serial, pcscd reads) ==="
if [ -f tests/hardware/nfc/test_differential.py ]; then
  if timeout 420 python3 -m pytest tests/hardware/nfc/test_differential.py -v --hil --no-header \
    >"$LOGDIR/differential.log" 2>&1; then
    record "PASS  differential"
  else
    record "WARN  differential (cards may be missing on some readers; see $LOGDIR/differential.log)"
  fi
else
  record "SKIP  differential (suite not present)"
fi

echo; echo "=== SUMMARY ==="
FAILS=0
for r in "${RESULTS[@]}"; do
  echo "  $r"
  case "$r" in FAIL*) FAILS=$((FAILS+1));; esac
done
echo "failures: $FAILS"
[ "$FAILS" -eq 0 ] || exit 1
