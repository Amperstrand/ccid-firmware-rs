#!/usr/bin/env bash
# Idempotently (re)create the ccid OTA places with their matches and
# labels (bolty-rs tools/hil/labgrid-place.sh pattern). The ai-legion
# coordinator persists places (StateDirectory=labgrid -> /var/lib/labgrid),
# so this normally only confirms shape — run it after a coordinator state
# loss or whenever acquire says "matches nothing".
#
# Places match NetworkService ONLY, on purpose: a place matching the
# USBSerialPort console tokens starts ser2net on acquire and OPENS the
# port — /dev/ttyACM0 DTR-resets the nucula (tools/hil/README.md §3).
set -euo pipefail

COORDINATOR="${LABGRID_COORDINATOR:-127.0.0.1:20408}"
EXPORTER_NAME="${LABGRID_EXPORTER_NAME:-ai-legion-ccid}"

lg() { labgrid-client -x "$COORDINATOR" -p "$1" "${@:2}"; }

ensure_place() {  # $1=place $2=comment $3...=tags, then -- matches...
    local place="$1" comment="$2"; shift 2
    local tags=()
    while [ "$#" -gt 0 ] && [ "$1" != "--" ]; do
        tags+=("$1"); shift
    done
    shift  # consume --
    if ! lg "$place" show >/dev/null 2>&1; then
        lg "$place" create
        echo "place $place created"
    fi
    local m
    for m in "$@"; do
        lg "$place" show | grep -qF "$m" || lg "$place" add-match "$m"
    done
    lg "$place" set-comment "$comment"
    lg "$place" set-tags "${tags[@]}"
    lg "$place" show | sed -n '1,6p'
}

ensure_place ccid-nucula \
    "nucula (ESP32-C3 + PN7160) OTA seat: acquire to flash via 192.168.13.230:3232 (host-tools/ota_push.py); console token exported, never auto-matched (DTR resets board)" \
    board=nucula transport=network \
    -- "${EXPORTER_NAME}/nucula/NetworkService"

ensure_place ccid-m5stick \
    "m5stick (ESP32 + MFRC522) OTA seat: acquire to flash via 192.168.13.234:3232 (host-tools/ota_push.py); console token exported, never auto-matched (DTR resets board)" \
    board=m5stick transport=network \
    -- "${EXPORTER_NAME}/m5stick/NetworkService"
