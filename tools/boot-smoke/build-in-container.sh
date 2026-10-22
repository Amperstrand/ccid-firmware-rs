#!/usr/bin/env bash
# In-container build for the boot-smoke gate. Runs INSIDE the ephemeral
# boot-smoke-esp32 image (see Dockerfile.esp32) with the PR checkout
# bind-mounted at /src and cache volumes at /target, /work/*, /out.
#
# THE ISOLATION CONTRACT: everything here — including the untrusted
# build.rs of the PR under test — executes confined to this container.
# Only files copied to /out cross back to the runner.
#
# Env (set by the workflow):
#   PR_NUMBER, HEAD_SHA   — recorded into manifest.json
# Expects: /src (PR checkout, read-write: esp-idf-sys writes .embuild),
#          /target (CARGO_TARGET_DIR), /out (artifact destination).
set -euo pipefail

PR_NUMBER="${PR_NUMBER:-0}"
HEAD_SHA="${HEAD_SHA:-unknown}"
BOARD="c3-ccid"   # the serving image the bench leg flashes (nucula c3)

echo "== boot-smoke in-container build: board=${BOARD} pr=${PR_NUMBER} head=${HEAD_SHA}"
test -d /src || { echo "FATAL: /src (PR checkout) not mounted" >&2; exit 2; }
test -w /out  || { echo "FATAL: /out not writable" >&2; exit 2; }

# espup's environment (installed into the image at build time).
# shellcheck disable=SC1091
source /root/export-esp.sh

cd /src/firmware/esp32-ccid

# The ordinary, proven bench build — same path build.sh c3-ccid always takes.
# Its own gates (dio flash-mode byte check, issue #309) run inside here.
rc=0
env CARGO_TARGET_DIR=/target ./build.sh "${BOARD}" || rc=$?
if [ "${rc}" -ne 0 ]; then
    echo "BOOT-SMOKE-BUILD-FAILED rc=${rc}" >&2
    exit "${rc}"
fi

# Locate the outputs exactly the way build.sh does (its glob is
# fingerprint-dependent; replicate the newest-match logic).
TARGET_DIR=/target
IMAGE="${TARGET_DIR}/esp32-ccid-${BOARD}.bin"
PART_TABLE="$(ls -t "${TARGET_DIR}"/*/debug/build/esp-idf-sys*/out/build/partition_table/partition-table.bin \
                    "${TARGET_DIR}"/*/debug/build/partition-table.bin 2>/dev/null | head -1 || true)"

[ -s "${IMAGE}" ] || { echo "FATAL: app image missing: ${IMAGE}" >&2; exit 3; }
[ -n "${PART_TABLE}" ] && [ -s "${PART_TABLE}" ] || { echo "FATAL: partition table not found under ${TARGET_DIR}" >&2; exit 3; }

# Emit the artifact set the workflow verifies + uploads.
sha256sum "${IMAGE}" | awk '{print $1"  '"$(basename "${IMAGE}")"'"}' > /out/esp32-ccid-c3-ccid.bin.sha256
sha256sum "${PART_TABLE}" | awk '{print $1"  partition-table.bin"}' > /out/partition-table.bin.sha256
cp "${IMAGE}" /out/esp32-ccid-c3-ccid.bin
cp "${PART_TABLE}" /out/partition-table.bin

python3 - <<PY > /out/manifest.json
import json, os, datetime
print(json.dumps({
    "schema": "amperstrand.boot-smoke-image/1",
    "pr": int(os.environ.get("PR_NUMBER", "0") or 0),
    "head_sha": os.environ.get("HEAD_SHA", "unknown"),
    "board": "${BOARD}",
    "image": "esp32-ccid-c3-ccid.bin",
    "built_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
}, indent=2))
PY

echo "== boot-smoke artifact set:"
ls -l /out
echo "BOOT-SMOKE-BUILD-OK"
