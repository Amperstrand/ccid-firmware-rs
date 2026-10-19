#!/usr/bin/env bash
# flash_and_test.sh — Build, flash, and test ESP32 CCID firmware
#
# FTDI FT232 wedge bug: espflash's UnixTightReset sends rapid DTR/RTS toggles
# via TIOCMSET ioctl which corrupts the FTDI chip's USB state machine.
# No software recovery exists — physical USB replug required every time.
# Tested: --baud 115200 (no baud change) still wedges. DTR/RTS is the trigger.
# See: esp-rs/espflash#646, serialport-rs#117, 2011 ftdi_sio.c kernel patch
#
# ALWAYS stop pcscd before flashing — it holds the serial port open.
#
# Usage:
#   ./flash_and_test.sh              # Build + flash + replug reminder + test
#   ./flash_and_test.sh --no-flash   # Skip flash, just start pcscd + test
#   ./flash_and_test.sh --test-only  # Only run pcsc_scan
#   BOARD=m5stick ./flash_and_test.sh ...  # Select board variant (default m5atom)
#
set -euo pipefail

# Non-interactive shells (cron, CI, ssh) lack ~/.cargo/bin on PATH
export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"
FIRMWARE="target/xtensa-esp32-espidf/release/esp32-ccid"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; }

# Detect the serial port from stable by-id identities — never hardcode
# /dev/ttyUSBx: USB re-enumeration renumbers it (FTDI unbind/rebind, replug).
# ESPFLASH_PORT always wins for explicit one-off targeting.
detect_port() {
    local matches=() p
    for p in /dev/serial/by-id/usb-Hades2001_M5stack_*-if00-port0 \
             /dev/serial/by-id/usb-FTDI_*-if00-port0; do
        [ -e "$p" ] && matches+=("$p")
    done
    case ${#matches[@]} in
        1) echo "${matches[0]}"; return 0 ;;
        0) return 1 ;;
        *)
            error "Ambiguous port detection (${#matches[@]} matches):"
            for p in "${matches[@]}"; do error "  $p"; done
            error "Set ESPFLASH_PORT=/dev/... explicitly."
            return 1 ;;
    esac
}
PORT="${ESPFLASH_PORT:-$(detect_port || true)}"
if [ -z "${PORT}" ]; then
    error "No serial port detected (no Hades2001/FTDI by-id device present)."
    error "Replug the board or set ESPFLASH_PORT=/dev/... explicitly."
    exit 1
fi
info "Serial port: ${PORT}"


build() {
    echo ""
    echo "=== Building firmware ==="
    # esp-idf-sys drops sdkconfig defaults silently unless ESP_IDF_SDKCONFIG_DEFAULTS
    # is set (metadata paths resolve against the registry package dir, not this repo —
    # see s_check_sdkconfig). Export script-relative paths so every machine gets the
    # 32KB main-task stack this firmware needs.
    export ESP_IDF_SDKCONFIG_DEFAULTS="${SCRIPT_DIR}/sdkconfig.defaults;${SCRIPT_DIR}/sdkconfig.defaults.esp32"
    # Board variant: BOARD=m5atom (default) or BOARD=m5stick
    local board="${BOARD:-m5atom}"
    info "Building for board: ${board} (override with BOARD=...)"
    cargo +esp build --manifest-path "${SCRIPT_DIR}/Cargo.toml" --release --features "backend-mfrc522,board-${board}" --no-default-features
    "${SCRIPT_DIR}/s_check_sdkconfig"
    info "Build complete (sdkconfig gate passed)."
}

flash() {
    echo ""
    echo "=== Flashing firmware (wedge-discipline tool) ==="
    if [ ! -f "${WORKSPACE_ROOT}/${FIRMWARE}" ]; then
        error "Firmware not found. Build first."
        exit 1
    fi

    # Wedge discipline (#299): pcscd quiesce under a held bench flock,
    # exporter-capped baud, writes-before-erases, USB/FTDI recovery
    # ladder, latency_timer restore, and the boot signature check —
    # all owned by esp32_flash.py. espflash is GONE from this path on
    # purpose: its DTR/RTS toggling is the documented FT232 wedge
    # (Gotcha #3) that used to force a physical replug after EVERY
    # flash here.
    local app_bin="/tmp/esp32-ccid-flash.$$.bin"
    esptool --chip esp32 elf2image -o "${app_bin}" \
        "${WORKSPACE_ROOT}/${FIRMWARE}" || {
            error "elf2image failed"; exit 1; }
    local part_table
    part_table="$(ls -t "${WORKSPACE_ROOT}/target/xtensa-esp32-espidf"/release/build/esp-idf-sys*/out/build/partition_table/partition-table.bin 2>/dev/null | head -1)"
    [ -z "${part_table}" ] && \
        part_table="${WORKSPACE_ROOT}/target/xtensa-esp32-espidf/release/build/partition-table.bin"

    python3 "${WORKSPACE_ROOT}/tests/hardware/labgrid/esp32_flash.py" \
        --port "$PORT" --chip esp32 --baud 115200 \
        --image 0x8000="${part_table}" --image 0x40000="${app_bin}" \
        --erase 0x30000=0x2000 \
        --exporter-cfg "${WORKSPACE_ROOT}/tests/hardware/labgrid/exporter-ai-legion-nfc.yaml" \
        || { error "Flash failed — see the tool verdict above."; exit 1; }
    rm -f "${app_bin}"

    info "Flash successful (boot-proven)."
}

start_pcscd() {
    echo ""
    echo "=== Starting pcscd ==="
    sudo rm -f /var/run/pcscd/pcscd.comm 2>/dev/null || true
    sudo systemctl restart pcscd.socket pcscd.service
    info "Waiting 8s for ESP32 boot + pcscd init..."
    sleep 8

    if sudo systemctl is-active --quiet pcscd; then
        info "pcscd is running."
    else
        error "pcscd failed to start. Check: sudo journalctl -u pcscd"
        exit 1
    fi
}

check_pcscd_log() {
    echo ""
    echo "=== pcscd log ==="
    sudo journalctl -u pcscd --no-pager -n 10 2>&1 | grep -E "Firmware|GemPC|Card|FAIL|reader|slot"
}

test_card() {
    echo ""
    echo "=== Running pcsc_scan (30s) ==="
    pcsc_scan -t 30 || true
}

NO_FLASH=0
TEST_ONLY=0

for arg in "$@"; do
    case "$arg" in
        --no-flash)  NO_FLASH=1 ;;
        --test-only) TEST_ONLY=1 ;;
        --help|-h)
            echo "Usage: $0 [--no-flash] [--test-only]"
            exit 0
            ;;
        *)
            error "Unknown option: $arg"
            exit 1
            ;;
    esac
done

if [ "$TEST_ONLY" -eq 1 ]; then
    start_pcscd
    check_pcscd_log
    test_card
    exit 0
fi

build

if [ "$NO_FLASH" -eq 0 ]; then
    flash
fi

start_pcscd
check_pcscd_log
test_card
