#!/usr/bin/env bash
# setup.sh — Install, flash, and configure ESP32 CCID reader
#
# Usage:
#   ./setup.sh              # Check prerequisites, install config, restart pcscd
#   ./setup.sh --flash      # Also flash firmware to ESP32
#   ./setup.sh --verify     # Just run pcsc_scan to verify
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"
CONFIG_SRC="${SCRIPT_DIR}/reader.conf"
CONFIG_DST="/etc/reader.conf.d/GemPCTwin.conf"
FIRMWARE="target/xtensa-esp32-espidf/release/esp32-ccid"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info()  { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error() { echo -e "${RED}[ERROR]${NC} $*"; }

check_prerequisites() {
    local missing=0

    echo ""
    echo "=== Checking prerequisites ==="

    # Wedge discipline (#299): the flash path rides esptool through
    # esp32_flash.py — espflash is deliberately absent (FT232 wedge).
    if ! command -v esptool &>/dev/null; then
        error "Missing prerequisite: esptool (pip install esptool)"
        missing=1
    else
        info "esptool found: $(esptool version 2>/dev/null | head -1)"
    fi
    if ! command -v python3 &>/dev/null; then
        warn "python3 not found — needed by esp32_flash.py (boot check)"
        missing=1
    fi

    if command -v pcscd &>/dev/null; then
        info "pcscd found: $(pcscd --version 2>/dev/null | head -1)"
    else
        error "pcscd not found. Install: sudo apt install pcscd libccid"
        missing=1
    fi

    if [ -f /usr/lib/pcsc/drivers/serial/libccidtwin.so ]; then
        info "libccidtwin.so found"
    else
        error "libccidtwin.so not found. Install: sudo apt install libccid"
        missing=1
    fi

    if command -v pcsc_scan &>/dev/null; then
        info "pcsc_scan found"
    else
        warn "pcsc_scan not found. Install: sudo apt install pcsc-tools"
        missing=1
    fi

    if [ "$missing" -ne 0 ]; then
        echo ""
        error "Missing prerequisites. Please install the above packages."
        exit 1
    fi

    echo ""
    info "All prerequisites satisfied."
}

flash_firmware() {
    echo ""
    echo "=== Flashing firmware ==="

    if [ ! -f "${WORKSPACE_ROOT}/${FIRMWARE}" ]; then
        error "Firmware not found at ${WORKSPACE_ROOT}/${FIRMWARE}"
        echo "  Build first: cargo build --release --target xtensa-esp32-espidf"
        exit 1
    fi

    # Wedge discipline (#299): delegate to the flash+test helper, which
    # routes through esp32_flash.py (held pcscd flock, capped baud,
    # ordered writes, recovery ladder, boot proof). Never a bare
    # espflash — its DTR/RTS toggling is the documented FT232 wedge.
    # no args = its documented full flow (build + flash + test); a
    # subprocess, not exec — setup continues to install_config after
    if [ -x "${SCRIPT_DIR}/flash_and_test.sh" ]; then
        "${SCRIPT_DIR}/flash_and_test.sh" || {
            error "flash_and_test.sh failed"; exit 1; }
        return
    fi
    error "flash_and_test.sh not found — use: ./build.sh <board> --flash"
    exit 1
}

install_config() {
    echo ""
    echo "=== Installing pcscd reader config ==="

    if [ ! -f "$CONFIG_SRC" ]; then
        error "reader.conf not found at ${CONFIG_SRC}"
        exit 1
    fi

    info "Copying ${CONFIG_SRC} -> ${CONFIG_DST}"
    sudo cp "$CONFIG_SRC" "$CONFIG_DST"
    info "Reader config installed."
}

restart_pcscd() {
    echo ""
    echo "=== Restarting pcscd ==="

    if sudo systemctl is-active --quiet pcscd 2>/dev/null; then
        info "Stopping pcscd..."
        sudo systemctl stop pcscd
    fi

    # pcscd may leave a stale socket after restart
    sudo rm -f /var/run/pcscd/pcscd.comm 2>/dev/null || true

    info "Starting pcscd..."
    sudo systemctl start pcscd

    if sudo systemctl is-active --quiet pcscd; then
        info "pcscd is running."
    else
        error "pcscd failed to start. Check: sudo journalctl -u pcscd"
        exit 1
    fi
}

verify() {
    echo ""
    echo "=== Verifying with pcsc_scan ==="

    if ! command -v pcsc_scan &>/dev/null; then
        warn "pcsc_scan not available. Install: sudo apt install pcsc-tools"
        return
    fi

    if ! sudo systemctl is-active --quiet pcscd; then
        warn "pcscd is not running. Start it first."
        return
    fi

    pcsc_scan
}

usage() {
    echo "Usage: $0 [--flash] [--verify]"
    echo ""
    echo "  (no flags)  Check prerequisites, install config, restart pcscd"
    echo "  --flash     Also flash firmware to ESP32"
    echo "  --verify    Run pcsc_scan to verify reader detection"
    echo "  --help      Show this help"
    exit 0
}

FLASH=0
VERIFY=0

for arg in "$@"; do
    case "$arg" in
        --flash)  FLASH=1 ;;
        --verify) VERIFY=1 ;;
        --help|-h) usage ;;
        *)
            error "Unknown option: $arg"
            usage
            ;;
    esac
done

check_prerequisites

if [ "$FLASH" -eq 1 ]; then
    flash_firmware
fi

install_config
restart_pcscd

if [ "$VERIFY" -eq 1 ]; then
    verify
else
    echo ""
    info "Setup complete. Run 'pcsc_scan' to verify, or: $0 --verify"
    info "Make sure the ESP32 is connected and powered on."
fi
