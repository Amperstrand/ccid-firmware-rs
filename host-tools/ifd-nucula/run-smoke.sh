#!/bin/bash
# ifd-nucula pcscd smoke test (issue #157). Run as ROOT via:
#     make smoke          (which wraps this in: unshare -m)
#
# Isolation: a tmpfs is mounted over /run/pcscd inside a private mount
# namespace, so the test pcscd gets a fresh socket and the SYSTEM pcscd
# (and any sibling lane using it) is never touched. All child processes
# are killed by captured PID — never pkill -f (a pattern string in any
# ancestor's command line self-matches and kills the harness).
set -u
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
SO="$SCRIPT_DIR/libifd-nucula.so"
MOCK="$SCRIPT_DIR/mock_board.py"
SMOKE="$SCRIPT_DIR/smoke_test.py"
PORT=29444
PCSCD_LOG=/tmp/ifd-nucula-smoke-pcscd.log
MOCK_LOG=/tmp/ifd-nucula-smoke-mock.log

[ -x "$SO" ] || { echo "missing $SO (run make first)"; exit 9; }

CONF=$(mktemp /tmp/ifd-nucula-smoke-conf.XXXXXX)
cat >"$CONF" <<EOF
FRIENDLYNAME "Nucula Mock (wifi IFD)"
DEVICENAME   127.0.0.1:$PORT
LIBPATH      $SO
CHANNELID    1
EOF

if ! grep -q " /run/pcscd " /proc/self/mountinfo; then
    mount -t tmpfs tmpfs /run/pcscd || { echo "mount failed (root + unshare -m required)"; exit 9; }
fi
rm -f /run/pcscd/pcscd.comm
unset PCSCLITE_CSOCK_NAME

MOCK_PID=""; PCSCD_PID=""
teardown() {
    [ -n "$PCSCD_PID" ] && kill "$PCSCD_PID" 2>/dev/null
    [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null
    rm -f "$CONF"
}
trap teardown EXIT

start_mock() {
    setsid python3 "$MOCK" "$PORT" --card "${NUCULA_MOCK_CARD:-legacy}" >>"$MOCK_LOG" 2>&1 </dev/null &
    MOCK_PID=$!
}

start_mock
sleep 0.5

/usr/sbin/pcscd -f -d -c "$CONF" >"$PCSCD_LOG" 2>&1 </dev/null &
PCSCD_PID=$!

up=0
for _ in $(seq 1 20); do
    if [ -S /run/pcscd/pcscd.comm ] && grep -q "Nucula Mock" "$PCSCD_LOG" 2>/dev/null; then
        up=1; break
    fi
    sleep 0.5
done
if [ "$up" != 1 ]; then
    echo "SETUP-FAIL: daemon or reader did not come up (see $PCSCD_LOG)"
    tail -20 "$PCSCD_LOG"
    exit 9
fi
echo "=== daemon up (namespace-isolated), reader loaded ==="

echo "=== PHASE 1: probe ==="
python3 "$SMOKE" probe; P1=$?

echo "=== PHASE 2: board death ==="
kill "$MOCK_PID" 2>/dev/null; MOCK_PID=""
sleep 0.5
python3 "$SMOKE" absent; P2=$?

echo "=== PHASE 3: board returns ==="
start_mock
sleep 0.5
python3 "$SMOKE" recover; P3=$?

teardown
echo "RESULT probe=$P1 absent=$P2 recover=$P3"
exit $(( P1 + P2 + P3 ))
