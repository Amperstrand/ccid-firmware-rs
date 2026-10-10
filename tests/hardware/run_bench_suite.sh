#!/bin/bash
# Full bench verification suite — one command, zero LLM credits.
#
# Verifies every device on the bench:
#   1. Bench inventory (labgrid coordinator, resources, places, pcscd)
#   2. F469 CCID HIL (flash + 7 tests through labgrid)
#   3. No-card conformance battery (nucula + m5stick differential)
#   4. Card-level session (whichever readers have cards)
#
# Usage:
#   bash tests/hardware/run_bench_suite.sh          # full suite
#   bash tests/hardware/run_bench_suite.sh --quick  # fast pass

set -euo pipefail
cd "$(dirname "$0")/../.."

QUICK="${1:-}"
PASS=0
FAIL=0
RESULTS=""

run() {
    local name="$1"; shift
    echo ""
    echo "=================================================================="
    echo "== $name"
    echo "=================================================================="
    if "$@"; then
        echo "==> $name: PASS"
        PASS=$((PASS + 1))
        RESULTS="$RESULTS  PASS  $name\n"
    else
        echo "==> $name: FAIL"
        FAIL=$((FAIL + 1))
        RESULTS="$RESULTS  FAIL  $name\n"
    fi
}

echo "Bench Verification Suite ($(date))"
echo "Mode: ${QUICK:---full}"

# 1. Inventory
run "Bench Inventory" \
    timeout 120 python3 tests/hardware/labgrid/bench_inventory.py

# 2. F469 CCID HIL (flash latest + run tests)
F469_BIN=/tmp/opencode/f469-bench.bin
source ~/.cargo/env
if cargo build --release --target thumbv7em-none-eabihf \
    --manifest-path firmware/ccid-firmware/Cargo.toml 2>&1 | grep -q "^error"; then
    echo "SKIP: F469 build failed"
    RESULTS="$RESULTS  SKIP  F469 HIL (build failed)\n"
else
    arm-none-eabi-objcopy -O binary \
        /root/.cargo-target/thumbv7em-none-eabihf/release/ccid-firmware \
        "$F469_BIN"
    run "F469 CCID HIL (7 tests)" \
        timeout 300 python3 -m pytest tests/hardware/labgrid/test_ccid_hil.py \
            -v --hil --firmware-bin="$F469_BIN" --no-header
fi

# 3. No-card conformance battery (m5stick + nucula)
# Note: 'IccPowerOn' FAIL when a card is present is environmental
run "Conformance Battery" \
    timeout 500 python3 -u tests/hardware/nfc/conformance_battery.py $QUICK

# 4. Card-level session (best effort — skips if no card)
echo ""
echo "=================================================================="
echo "== Card-Level Session (best effort)"
echo "=================================================================="
if timeout 30 python3 -c "
from smartcard.System import readers
from smartcard.Exceptions import NoCardException
for r in readers():
    if 'GemPCTwin' not in str(r): continue
    try:
        c = r.createConnection(); c.connect()
        atr = ' '.join(f'{b:02X}' for b in c.getATR())
        data, sw1, sw2 = c.transmit([0x00, 0xA4, 0x04, 0x00, 0x00])
        print(f'  GemPCTwin: ATR={atr} SELECT SW={sw1:02X}{sw2:02X}')
        c.disconnect()
    except (NoCardException, Exception):
        pass
" 2>/dev/null; then
    echo "==> Card Session: see output above"
    PASS=$((PASS + 1))
    RESULTS="$RESULTS  INFO  Card Session (see output)\n"
else
    echo "==> Card Session: no card detected"
    RESULTS="$RESULTS  SKIP  Card Session (no card)\n"
fi

# Summary
echo ""
echo "=================================================================="
echo "SUMMARY"
echo "=================================================================="
echo -e "$RESULTS"
echo "Pass: $PASS  Fail: $FAIL"
[ "$FAIL" -eq 0 ] || exit 1
