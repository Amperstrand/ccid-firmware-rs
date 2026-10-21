#!/usr/bin/env bash
# Build + deploy the Amperstrand-patched libifdvpcd (issue #326 host half).
#
# Two upstream bugs fixed (see libifdvpcd-amperstrand.patch and
# docs in this directory):
#   1. A zero-length ATR answer (the vpcd protocol's "no card" signal)
#      triggered vicc_eject() — connection churn that prevents pcscd
#      from ever seeing a slow-to-warm vpicc (our wifi firmware).
#   2. recvall() has no receive timeout and the listener uses backlog 0
#      — one unanswered probe wedges pcscd's EH thread forever and all
#      future vpicc connections are dropped.
#
# Usage:
#   ./build_libifdvpcd_amperstrand.sh build  [upstream_pin]
#   ./build_libifdvpcd_amperstrand.sh deploy [reader_conf]
#   ./build_libifdvpcd_amperstrand.sh restore [reader_conf]
#
# build   clones upstream vsmartcard at the pinned commit (default:
#         93286a90d2411120ae17299f9902e1fdd8954185), applies the patch,
#         compiles libifdvpcd-amperstrand.so into this directory.
# deploy  installs the .so to /usr/local/lib/pcsc/drivers/serial/ and
#         swaps LIBPATH in the reader config (default:
#         /etc/reader.conf.d/vpcd-nucula-35967). pcscd must be
#         restarted afterwards (under the bench flock).
# restore points the reader config back at the stock library.
#
# VPCDSLOTS=2 matches the Ubuntu-packaged build (8 slots would rename
# the reader set to 00 00..00 07 and churn every pinned-reader script).
set -euo pipefail

UPSTREAM_PIN="${2:-93286a90d2411120ae17299f9902e1fdd8954185}"
# accept pin as $1 when only one arg form is used
[ "${1:-}" != "build" ] || true
if [ "${1:-}" = "build" ] && [ "${2:-}" != "" ]; then UPSTREAM_PIN="$2"; fi
READER_CONF="${3:-/etc/reader.conf.d/vpcd-nucula-35967}"
if [ "${2:-}" != "" ] && [ ! -f "$READER_CONF" ] && [[ ! "$2" =~ ^[0-9a-f]{40}$ ]]; then READER_CONF="$2"; fi

HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/upstream-vsc"
OUT="$HERE/libifdvpcd-amperstrand.so"
STOCK="/usr/lib/pcsc/drivers/serial/libifdvpcd.so"
DEST_DIR="/usr/local/lib/pcsc/drivers/serial"
DEST="$DEST_DIR/libifdvpcd-amperstrand.so"

cmd="${1:-}"
case "$cmd" in
  build)
    rm -rf "$SRC"
    git clone --quiet https://github.com/frankmorgner/vsmartcard.git "$SRC"
    git -C "$SRC" checkout --quiet "$UPSTREAM_PIN"
    git -C "$SRC" apply "$HERE/libifdvpcd-amperstrand.patch"
    gcc -O2 -fPIC -shared \
      -DHAVE_DECL_MSG_NOSIGNAL=1 -DHAVE_DECL_SO_NOSIGPIPE=0 \
      -DPACKAGE_VERSION='"3.3+amperstrand1"' -DVPCDSLOTS=2 \
      -o "$OUT" \
      "$SRC/virtualsmartcard/src/vpcd/vpcd.c" \
      "$SRC/virtualsmartcard/src/vpcd/lock.c" \
      "$SRC/virtualsmartcard/src/ifd-vpcd/ifd-vpcd.c" \
      -I/usr/include/PCSC \
      -I"$SRC/virtualsmartcard/src/vpcd" \
      -I"$SRC/virtualsmartcard/src/ifd-vpcd" \
      -lpthread
    echo "built: $OUT (upstream $UPSTREAM_PIN + amperstrand patch)"
    ;;
  deploy)
    [ -f "$OUT" ] || { echo "run 'build' first" >&2; exit 1; }
    mkdir -p "$DEST_DIR"
    install -m 644 "$OUT" "$DEST"
    sed -i "s|^LIBPATH .*|LIBPATH $DEST|" "$READER_CONF"
    echo "deployed: $DEST ; LIBPATH swapped in $READER_CONF"
    echo "restart pcscd to activate (under the bench flock)."
    ;;
  restore)
    sed -i "s|^LIBPATH .*|LIBPATH $STOCK|" "$READER_CONF"
    echo "restored LIBPATH to stock in $READER_CONF"
    echo "restart pcscd to activate (under the bench flock)."
    ;;
  *)
    echo "usage: $0 {build|deploy|restore} [upstream_pin|reader_conf]" >&2
    exit 2
    ;;
esac
