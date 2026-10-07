#!/usr/bin/env bash
# build.sh — bench build (+ optional esptool flash) for the three esp32-ccid
# hardware variants, encoding the verified ai-legion incantations from the
# "ESP-IDF Rust toolchain and bring-up builds" section of README.md.
#
# This is the bench flow (sdkconfig.full / sdkconfig-xtensa.full + esptool).
# For the CI-parity defaults flow (sdkconfig.defaults*) plus pcscd testing use
# flash_and_test.sh. s_check_sdkconfig is deliberately NOT run here: the
# .full configs carry different values than that gate expects.
#
# Usage:
#   ./build.sh <c3|m5stick|m5atom> [--flash <port>] [--dry-run]
#
# NUCULA_WIFI_SSID / NUCULA_WIFI_PASS pass through from the environment
# (baked in at compile time via option_env!; build.rs reruns on change).
set -euo pipefail

# Non-interactive shells (cron, CI, ssh) lack ~/.cargo/bin on PATH
export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Bench default: global ~/.cargo/config.toml pins target-dir to ~/.cargo-target
# (shared across checkouts); honor an explicit CARGO_TARGET_DIR if set.
TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cargo-target}"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }

usage() {
    cat <<EOF
Usage: $0 <c3|m5stick|m5atom> [--flash <port>] [--dry-run]

Boards:
  c3       ESP32-C3 nucula, PN7160 bring-up (nightly toolchain, debug build,
           sdkconfig.full, ota_0 slot @ 0x40000, baud 460800)
  m5stick  ESP32 M5StickC, MFRC522 CCID (esp toolchain, release,
           sdkconfig-xtensa.full, factory slot @ 0x30000, baud 115200)
  m5atom   ESP32 M5Stack Atom, MFRC522 CCID (as m5stick, default features)

Options:
  --flash <port>  esptool write-flash after a successful build
  --dry-run       print the commands without executing anything
  -h, --help      show this help

Env:
  CARGO_TARGET_DIR             honored; defaults to ~/.cargo-target
  NUCULA_WIFI_SSID/_PASS       passed through (baked in at build time)
EOF
}

DRY_RUN=0
FLASH_PORT=""

BOARD="${1:-}"
if [ -z "${BOARD}" ] || [ "${BOARD}" = "-h" ] || [ "${BOARD}" = "--help" ]; then
    usage
    [ -n "${BOARD}" ] && exit 0
    exit 1
fi
shift

while [ $# -gt 0 ]; do
    case "$1" in
        --flash)
            [ $# -ge 2 ] || { echo "error: --flash requires a port argument" >&2; exit 1; }
            FLASH_PORT="$2"
            shift 2
            ;;
        --dry-run)
            DRY_RUN=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown option: $1" >&2
            usage
            exit 1
            ;;
    esac
done

case "${BOARD}" in
    c3)
        TOOLCHAIN="nightly"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig.full"
        TRIPLE="riscv32imc-esp-espidf"
        PROFILE="debug"
        RELEASE_ARGS=()
        FEATURE_ARGS=(--no-default-features --features pn7160-bringup,pn7160-verdict-b)
        CHIP="esp32c3"
        FLASH_OFFSET="0x40000"
        FLASH_BAUD="460800"
        ;;
    m5stick)
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig-xtensa.full"
        TRIPLE="xtensa-esp32-espidf"
        PROFILE="release"
        RELEASE_ARGS=(--release)
        FEATURE_ARGS=(--no-default-features --features backend-mfrc522,board-m5stick)
        CHIP="esp32"
        FLASH_OFFSET="0x30000"
        FLASH_BAUD="115200"
        ;;
    m5atom)
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig-xtensa.full"
        TRIPLE="xtensa-esp32-espidf"
        PROFILE="release"
        RELEASE_ARGS=(--release)
        FEATURE_ARGS=()
        CHIP="esp32"
        FLASH_OFFSET="0x30000"
        FLASH_BAUD="115200"
        ;;
    *)
        echo "error: unknown board: ${BOARD} (expected c3, m5stick, or m5atom)" >&2
        exit 1
        ;;
esac

ELF="${TARGET_DIR}/${TRIPLE}/${PROFILE}/esp32-ccid"
IMAGE="${TARGET_DIR}/esp32-ccid-${BOARD}.bin"

# Print a command; execute it unless --dry-run.
run() {
    echo "+ $*"
    [ "${DRY_RUN}" -eq 1 ] && return 0
    "$@"
}

info "board=${BOARD} target=${TRIPLE} profile=${PROFILE} chip=${CHIP}"
info "CARGO_TARGET_DIR=${TARGET_DIR}"
if [ -n "${NUCULA_WIFI_SSID:-}" ]; then
    info "NUCULA_WIFI_SSID: set (inherited)"
else
    warn "NUCULA_WIFI_SSID: unset — credential-baking crates will compile without it"
fi

echo "+ cd ${SCRIPT_DIR}"
[ "${DRY_RUN}" -eq 1 ] || cd "${SCRIPT_DIR}"

echo "+ . ${HOME}/export-esp.sh"
if [ "${DRY_RUN}" -eq 0 ]; then
    [ -f "${HOME}/export-esp.sh" ] || { echo "error: ${HOME}/export-esp.sh missing — run espup install" >&2; exit 1; }
    # shellcheck disable=SC1091
    . "${HOME}/export-esp.sh"
fi

# Partition CSV injection: ESP-IDF cmake resolves
# CONFIG_PARTITION_TABLE_CUSTOM_FILENAME against the esp-idf project dir
# (the embuild out dir, hash-named). The esp-idf-sys build script copies
# ESP_IDF_GLOB_* matches into that dir BEFORE cmake configure
# (BUILD-OPTIONS.md), so exporting these replaces the old manual
# post-configure copies — and works on fresh checkouts.
export ESP_IDF_GLOB_PARTCSV_BASE="${SCRIPT_DIR}"
export ESP_IDF_GLOB_PARTCSV_1="/partitions-ota.csv"

run env RUSTUP_TOOLCHAIN="${TOOLCHAIN}" ESP_IDF_SDKCONFIG="${SDKCONFIG}" \
    cargo build --target "${TRIPLE}" "${RELEASE_ARGS[@]}" "${FEATURE_ARGS[@]}"

# esptool cannot flash a raw cargo ELF ("will not fit in flash") — convert first.
run esptool --chip "${CHIP}" elf2image -o "${IMAGE}" "${ELF}"

if [ -n "${FLASH_PORT}" ]; then
    # c3 note: if partitions-ota.csv changed, the partition table itself must
    # be reflashed too (write-flash 0x8000 <target>/partition-table.bin).
    run esptool --chip "${CHIP}" -p "${FLASH_PORT}" --baud "${FLASH_BAUD}" \
        write-flash "${FLASH_OFFSET}" "${IMAGE}"
    if [ "${CHIP}" = "esp32" ]; then
        warn "FTDI DTR/RTS wedge: physically replug the M5Stack board before expecting"
        warn "serial communication to work again (see flash_and_test.sh header)."
    fi
fi

info "done: ${IMAGE}"
