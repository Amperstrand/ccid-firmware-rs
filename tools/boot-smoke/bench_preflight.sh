#!/usr/bin/env bash
# Bench-leg preflight for the boot-smoke gate. Runs on the self-hosted
# bench runner BEFORE any flash. Pure checks — acquires nothing, changes
# nothing; every FAIL is a hard stop with an actionable message.
set -uo pipefail

fail=0
need() { # need <label> <condition-result>
    if [ "$2" -eq 0 ]; then echo "PASS  $1"; else echo "FAIL  $1"; fail=1; fi
}

# --- required environment (workflow job env) ---
for v in NUCULA_PORT FLASH_BAUD PART_TABLE_OFFSET APP_OFFSET OTADATA_OFFSET KNOWN_GOOD_IMAGE_REF; do
    if [ -n "${!v:-}" ]; then echo "PASS  env ${v} set (${!v})"; else echo "FAIL  env ${v} unset"; fail=1; fi
done

# --- offsets must parse as hex, app must sit past the table ---
python3 - <<'PY' && echo "PASS  offsets sane" || { echo "FAIL  offsets insane"; fail=1; }
import os, sys
g = lambda k: int(os.environ[k], 16)
t, a, o = g("PART_TABLE_OFFSET"), g("APP_OFFSET"), g("OTADATA_OFFSET")
sys.exit(0 if (t < o < a and a % 0x1000 == 0) else 1)
PY

# --- the DUT port is present ---
[ -c "${NUCULA_PORT:-/dev/null}" ] && echo "PASS  DUT port present" || { echo "FAIL  DUT port missing: ${NUCULA_PORT:-unset}"; fail=1; }

# --- bench tooling this job drives (from the MAIN checkout, #183 rule) ---
[ -f tests/hardware/labgrid/esp32_flash.py ] && echo "PASS  esp32_flash.py" || { echo "FAIL  esp32_flash.py"; fail=1; }
[ -f tests/hardware/labgrid/gempc_smoke.py ] && echo "PASS  gempc_smoke.py" || { echo "FAIL  gempc_smoke.py"; fail=1; }
[ -f tests/hardware/labgrid/exporter-ai-legion-nfc.yaml ] && echo "PASS  exporter cfg" || { echo "FAIL  exporter cfg"; fail=1; }
/usr/bin/python3 -c 'import serial' 2>/dev/null && echo "PASS  pyserial (/usr/bin/python3)" || { echo "FAIL  pyserial missing on /usr/bin/python3"; fail=1; }

# --- the quiesce target exists on this host ---
systemctl cat nucula-ccid-broker.service >/dev/null 2>&1 && echo "PASS  nucula-ccid-broker unit" || { echo "FAIL  nucula-ccid-broker unit unknown"; fail=1; }

# --- operator kill-switch (skip-with-notice; see bench_window.sh) ---
MARKER=/tmp/amperstrand-bench-ci.disabled
if [ -e "$MARKER" ]; then
    echo "NOTICE: bench CI disabled by operator — $(cat "$MARKER" 2>/dev/null || echo 'no reason given')"
    echo "BENCH-CI-SKIPPED (operator) — preflight passes trivially; the window step will skip"
    echo "BOOT-SMOKE-PREFLIGHT-OK"
    exit 0
fi

# --- labgrid coordinator reachable + the DUT place exists (the CI queue) ---
if command -v labgrid-client >/dev/null 2>&1 \
   && timeout 15 labgrid-client -p "${BENCH_PLACE:-nucula-c3}" show >/dev/null 2>&1; then
    echo "PASS  labgrid place '${BENCH_PLACE:-nucula-c3}' visible (reservation queue available)"
else
    echo "FAIL  labgrid coordinator unreachable or place '${BENCH_PLACE:-nucula-c3}' missing — bench_window.sh cannot queue"
    fail=1
fi

# --- the cross-project bench lock is free right now (peek, don't hold) ---
if flock -n /tmp/amperstrand-bench.lock -c true 2>/dev/null; then
    echo "PASS  bench flock free"
else
    echo "FAIL  bench flock held by another lane — do not start a flash leg"
    fail=1
fi

echo "----"
if [ "${fail}" -eq 0 ]; then echo "BOOT-SMOKE-PREFLIGHT-OK"; else echo "BOOT-SMOKE-PREFLIGHT-FAILED"; fi
exit "${fail}"
