#!/usr/bin/env bash
# switch_role.sh — flip the M5Stick lab rig between its two supported roles:
#   bolty : bolty-esp32 tollgate/console firmware + bolty-console daemon (serial)
#   ccid  : esp32-ccid GemPC-Twin reader firmware + pcscd via libccidtwin
#
# Env vars:
#   VARIANT     : esp32-ccid features for the ccid role (default: backend-mfrc522,board-m5stick)
#   BOLTY_FEATURES : bolty-esp32 features for the bolty role (default: board-m5stick)
#
# The two roles fight over the same FT232 serial port, so switching must
# orchestrate: daemon stop/start, pcscd reader.conf enable/disable, rebuild,
# flash (esptool @115200 — espflash's DTR/RTS toggles wedge this bridge),
# and a post-switch health verification. See docs/role-switch.md.
set -euo pipefail
export PATH="$HOME/.cargo/bin:$PATH"

ROLE="${1:-}"
VARIANT="${VARIANT:-backend-mfrc522,board-m5stick}"   # esp32-ccid features for the ccid role
BOLTY_FEATURES="${BOLTY_FEATURES:-board-m5stick}"      # bolty-esp32 features for the bolty role
STICK_BY_ID="/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"
PORT="${PORT:-$([ -e "$STICK_BY_ID" ] && echo "$STICK_BY_ID" || echo /dev/ttyUSB0)}"
READER_CONF=/etc/reader.conf.d/esp32-ccid
CCID_REPO=${CCID_REPO:-$HOME/src/ccid-firmware-rs}
BOLTY_REPO=${BOLTY_REPO:-$HOME/src/bolty-rs}

say() { printf '\n=== %s ===\n' "$*"; }
usb_rescan() {
  say "USB rebind (clears FT232 wedge states)"
  echo -n 1-1 | sudo tee /sys/bus/usb/drivers/usb/unbind >/dev/null; sleep 4
  echo -n 1-1 | sudo tee /sys/bus/usb/drivers/usb/bind   >/dev/null; sleep 6
}
flash_merged() {  # $1 = merged bin
  # Wedge discipline (#299): route through esp32_flash.py — held pcscd
  # flock, recovery ladder, boot proof. A merged image carries the
  # bootloader at 0x0, so no separate PT/otadata steps apply here.
  sudo python3 "$(dirname "$0")/../tests/hardware/labgrid/esp32_flash.py" \
    --port "$PORT" --chip esp32 --baud 115200 --image 0x0="$1"
}

case "$ROLE" in
  ccid)
    say "stopping bolty-console (frees the port)"
    sudo systemctl stop bolty-console
    say "enabling pcscd serial reader config"
    sudo mv "${READER_CONF}.disabled" "$READER_CONF" 2>/dev/null || true
    say "building esp32-ccid ($VARIANT)"
    (cd "$CCID_REPO/firmware/esp32-ccid" && \
      ESP_IDF_SDKCONFIG_DEFAULTS="$CCID_REPO/firmware/esp32-ccid/sdkconfig.defaults;$CCID_REPO/firmware/esp32-ccid/sdkconfig.defaults.esp32" \
      cargo +esp build --release --no-default-features --features "$VARIANT" >/dev/null && \
      ESP_IDF_SDKCONFIG_DEFAULTS="$CCID_REPO/firmware/esp32-ccid/sdkconfig.defaults;$CCID_REPO/firmware/esp32-ccid/sdkconfig.defaults.esp32" \
      cargo +esp espflash save-image --chip esp32 --release --no-default-features \
        --features "$VARIANT" --merge /tmp/esp32-ccid-merged.bin >/dev/null)
    usb_rescan
    flash_merged /tmp/esp32-ccid-merged.bin
    say "restarting pcscd (firmware NFC init races a fast probe — retry once)"
    sudo systemctl restart pcscd.socket pcscd.service; sleep 10
    say "verify: PC/SC readers"
    if ! python3 -c 'from smartcard.System import readers; rs=[str(r) for r in readers()]; print(rs); exit(0 if rs else 1)'; then
      sudo systemctl restart pcscd.service; sleep 8
      python3 -c 'from smartcard.System import readers; rs=[str(r) for r in readers()]; print(rs); exit(0 if rs else 1)'
    fi
    echo "ROLE=ccid active. Reader: ESP32 CCID Serial. Use host-tools/boltwipe.py."
    ;;
  bolty)
    say "disabling pcscd serial reader config"
    sudo mv "$READER_CONF" "${READER_CONF}.disabled" 2>/dev/null || true
    sudo systemctl restart pcscd.socket pcscd.service 2>/dev/null || true
    say "stopping any port holders"
    sudo systemctl stop bolty-console 2>/dev/null || true
    say "building bolty-esp32 ($BOLTY_FEATURES)"
    (cd "$BOLTY_REPO/apps/bolty-esp32" && \
      cargo +esp build --release --features "$BOLTY_FEATURES" >/dev/null && \
      cargo +esp espflash save-image --chip esp32 --release --features "$BOLTY_FEATURES" \
        --merge /tmp/bolty-merged.bin >/dev/null)
    usb_rescan
    flash_merged /tmp/bolty-merged.bin
    say "starting bolty-console daemon"
    sudo systemctl start bolty-console; sleep 6
    say "verify: daemon PING"
    python3 "$BOLTY_REPO/tools/hil/bolty-ctl.py" PING
    echo "ROLE=bolty active. HIL: python3 $BOLTY_REPO/tools/hil/burn_cycle.py"
    ;;
  *)
    echo "usage: $0 bolty|ccid   (ccid variant via VARIANT=...)" >&2
    exit 2
    ;;
esac
