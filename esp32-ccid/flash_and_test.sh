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
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIRMWARE="target/xtensa-esp32-espidf/release/esp32-ccid"
PORT="${ESPFLASH_PORT:-/dev/ttyUSB0}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; }

build() {
    echo ""
    echo "=== Building firmware ==="
    cargo +esp build --release --features backend-mfrc522
    info "Build complete."
}

flash() {
    echo ""
    echo "=== Stopping pcscd ==="
    sudo systemctl stop pcscd.socket pcscd.service 2>/dev/null || true
    sleep 1

    echo ""
    echo "=== Flashing firmware ==="
    if [ ! -f "${SCRIPT_DIR}/${FIRMWARE}" ]; then
        error "Firmware not found. Build first."
        exit 1
    fi

    if ! espflash flash --port "$PORT" --after no-reset-no-stub "${SCRIPT_DIR}/${FIRMWARE}"; then
        error "Flash failed. Is the ESP32 connected at ${PORT}?"
        exit 1
    fi

    info "Flash successful."
    echo ""
    warn ">>> PHYSICAL USB RECONNECT REQUIRED <<<"
    warn "The FTDI FT232 chip is corrupted by espflash's DTR/RTS toggles."
    warn "1. Unplug the M5Stack USB cable"
    warn "2. Wait 5 seconds"
    warn "3. Reconnect — ESP32 will boot the new firmware"
    warn "4. Re-run: $0 --no-flash"
    exit 0
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
