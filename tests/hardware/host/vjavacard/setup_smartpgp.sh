#!/usr/bin/env bash
# Setup + launch helper for vjavacard SmartPGP mode (#261 tier b+).
#
# SmartPGP mode needs the compiled applet classes from the pinned CAP
# artifact. The classes are bench state (created ad-hoc by the GP
# lifecycle lane 2026-10-22 at /opt/bench/vjavacard/smartpgp-classes);
# this script makes the path reproducible: verify the pinned sha,
# extract APPLET-INF/classes, print (or exec, with --run) the exact
# bridge launch line.
#
# Usage:
#   ./setup_smartpgp.sh                # verify + extract + print line
#   ./setup_smartpgp.sh --run [port]   # ... then exec the bridge
#
# Env:
#   SMARTPGP_CAP   path to the .cap (ant-javacard JAR)
#                  (default: the bench .omo pin below)
#   SMARTPGP_DIR   extraction target (default: ./smartpgp-classes)
set -euo pipefail

PINNED_SHA256="9213b1d6d53764b00b0752028b491971e1b210c768643434fc05de82acba7422"
DEFAULT_CAP="/root/src/ccid-firmware-rs/.omo/gp-artifacts/SmartPGP-v1.23.0-javacard-3.0.4/SmartPGPApplet-rsa_up_to_4096.cap"

CAP="${SMARTPGP_CAP:-$DEFAULT_CAP}"
OUT="${SMARTPGP_DIR:-$(pwd)/smartpgp-classes}"
PORT="${2:-35973}"
FQCN="fr.anssi.smartpgp.SmartPGPApplet"
AID="D276000124010304AFAF000000000000"

[ -f "$CAP" ] || {
    echo "CAP not found: $CAP" >&2
    echo "Set SMARTPGP_CAP; the pin + provenance live in" >&2
    echo "tests/hardware/nfc/J3R180-RUNBOOK.md (sha $PINNED_SHA256)" >&2
    exit 1
}

echo "verifying $(basename "$CAP") ..."
GOT=$(sha256sum "$CAP" | awk '{print $1}')
if [ "$GOT" != "$PINNED_SHA256" ]; then
    echo "SHA MISMATCH — refusing:" >&2
    echo "  want $PINNED_SHA256" >&2
    echo "  got  $GOT" >&2
    exit 1
fi
echo "sha ok ($PINNED_SHA256)"

rm -rf "$OUT"
TMP="$(mktemp -d)"
unzip -q -o "$CAP" 'APPLET-INF/classes/*' -d "$TMP"
mkdir -p "$OUT"
mv "$TMP"/APPLET-INF/classes/* "$OUT"/
rm -rf "$TMP"
test -d "$OUT/fr" || { echo "extraction produced no classes at $OUT/fr" >&2; exit 1; }
echo "classes at $OUT (classpath root — classpath wants $OUT, not APPLET-INF)"

HERE="$(cd "$(dirname "$0")" && pwd)"
JAR_DIR="${JC_SIM_DIR:-$HOME/.m2/repository/com/licel/jcardsim/2.2.2}"
SIM="$JAR_DIR/jcardsim-2.2.2-unsigned.jar"
if [ ! -f "$SIM" ]; then
    echo "unsigned jcardsim copy missing ($SIM) — see test_vjavacard.py docstring" >&2
    exit 1
fi

LAUNCH="java -cp $HERE/target/classes:$SIM:$OUT org.amperstrand.VpcdBridge 127.0.0.1 $PORT --applet $FQCN $AID"
echo
echo "launch line:"
echo "  $LAUNCH"
echo

if [ "${1:-}" = "--run" ]; then
    cd "$HERE"
    exec $LAUNCH
fi
