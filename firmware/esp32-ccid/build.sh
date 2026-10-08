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

# The esp-idf-sys build script locates the workspace by popping its OUT_DIR
# six levels — with a shared/global CARGO_TARGET_DIR that resolves INSIDE the
# target dir, not to this checkout, and the build silently pins whatever
# ESP-IDF the stale state there says (the bench built v5.2.3 for a full day
# while the manifest said v5.2.4/v5.5.1). Pinning the workspace explicitly
# makes the [package.metadata.esp-idf-sys] esp_idf_version pin authoritative.
export CARGO_WORKSPACE_DIR="${SCRIPT_DIR}/../.."

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
Usage: $0 <c3|c3-ccid|m5stick|m5atom> [--flash <port>] [--dry-run]

Boards:
  c3       ESP32-C3 nucula, PN7160 bring-up (nightly toolchain, debug build,
           sdkconfig.full, ota_0 slot @ 0x40000, baud 460800)
  c3-ccid  ESP32-C3 nucula, PN7160 CCID over USB-CDC (as c3; the full
           reader firmware — flash + host pcscd via libccidtwin)
  m5stick  ESP32 M5StickC, MFRC522 CCID (esp toolchain, release,
           sdkconfig-xtensa.full, ota_0 slot @ 0x40000, baud 115200)
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
    c3-ccid)
        TOOLCHAIN="nightly"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig.full"
        TRIPLE="riscv32imc-esp-espidf"
        PROFILE="debug"
        RELEASE_ARGS=()
        FEATURE_ARGS=(--no-default-features --features pn7160-ccid)
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
        FLASH_OFFSET="0x40000"  # ota_0 slot per partitions-ota.csv (Codex #79 P1)
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
        FLASH_OFFSET="0x40000"  # ota_0 slot per partitions-ota.csv (Codex #79 P1)
        FLASH_BAUD="115200"
        ;;
    *)
        echo "error: unknown board: ${BOARD} (expected c3, c3-ccid, m5stick, or m5atom)" >&2
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

# Partition CSV injection. Two flows, two mechanisms (Codex review on #76):
# - c3: sdkconfig.full references partitions-ota.csv by its REAL name; the
#   esp-idf-sys build script copies ESP_IDF_GLOB_* matches into the project
#   dir BEFORE cmake configure (BUILD-OPTIONS.md) — fresh-checkout safe.
# - xtensa: sdkconfig-xtensa.full expects a RENAMED partitions.csv, and the
#   GLOB copy cannot rename — so the old renamed copies are restored
#   (target-dir root + existing esp-idf-sys out dirs).
# Full-sdkconfig change detection (Codex reviews on #73/#79): cmake caches
# the configure; a stale CMakeCache.txt silently builds with the PREVIOUS
# sdkconfig. A stamp records the last-applied sdkconfig — when it differs
# (or when caches exist but no stamp does — first run against a shared
# target dir predating this scheme), the caches are dropped before cargo
# runs so the new config is real.
STAMP="${TARGET_DIR}/${BOARD}.sdkconfig.stamp"
if [ "${DRY_RUN}" -eq 0 ]; then
    STALE=0
    if [ ! -f "${STAMP}" ]; then
        # No stamp + existing esp-idf-sys caches = unknown provenance.
        for cache in "${TARGET_DIR}/${TRIPLE}"/*/build/esp-idf-sys*/out/build/CMakeCache.txt; do
            [ -f "${cache}" ] && STALE=1
        done
    elif ! cmp -s "${SDKCONFIG}" "${STAMP}"; then
        STALE=1
    fi
    if [ "${STALE}" -eq 1 ]; then
        info "sdkconfig stale/unstamped — dropping existing CMake caches"
        for cache in "${TARGET_DIR}/${TRIPLE}"/*/build/esp-idf-sys*/out/build/CMakeCache.txt; do
            [ -f "${cache}" ] || continue
            rm -f "${cache}"
        done
    fi
fi

if [ "${CHIP}" = "esp32c3" ]; then
    export ESP_IDF_GLOB_PARTCSV_BASE="${SCRIPT_DIR}"
    export ESP_IDF_GLOB_PARTCSV_1="/partitions-ota.csv"
else
    echo "+ cp ${SCRIPT_DIR}/partitions-ota.csv ${TARGET_DIR}/partitions.csv (+ esp-idf-sys out dirs)"
    if [ "${DRY_RUN}" -eq 0 ]; then
        mkdir -p "${TARGET_DIR}"
        cp "${SCRIPT_DIR}/partitions-ota.csv" "${TARGET_DIR}/partitions.csv"
        for out_dir in "${TARGET_DIR}/${TRIPLE}"/*/build/esp-idf-sys*/out; do
            [ -d "${out_dir}" ] || continue
            cp "${SCRIPT_DIR}/partitions-ota.csv" "${out_dir}/partitions.csv"
        done
    fi
fi

# Shared-target-dir cmake cache guard (bench trap, 2026-10-08): the global
# ~/.cargo/config.toml target-dir is shared by every checkout/worktree. The
# esp-idf-sys cmake cache records ABSOLUTE IDF paths under the building
# checkout's .embuild — reusing another checkout's cache fails with
# "CMake Error: The source ... does not match the source ... used to generate
# cache", and the stale ELF + piped-rc habits can silently flash OLD firmware.
# Both cargo layouts are scanned (esp-idf-sys-<hash> AND esp-idf-sys/<hash>);
# contamination = a CMakeCache pointing elsewhere, or any .cmake reference
# to a foreign .embuild path (survives in CMakeFiles even without a cache).
if [ "${DRY_RUN}" -eq 0 ]; then
    for out_build in "${TARGET_DIR}"/{,${TRIPLE}}/{debug,release}/build/esp-idf-sys{,-}*/out/build; do
        [ -d "${out_build}" ] || continue
        contaminated=""
        if [ -f "${out_build}/CMakeCache.txt" ]; then
            idf_src="$(grep -m1 '^esp-idf_SOURCE_DIR:STATIC=' "${out_build}/CMakeCache.txt" | cut -d= -f2)"
            case "${idf_src}" in
                "${CARGO_WORKSPACE_DIR}"/.embuild/*) ;;
                *) contaminated="CMakeCache esp-idf_SOURCE_DIR=${idf_src:-<empty>}" ;;
            esac
        fi
        if [ -z "${contaminated}" ] && grep -rIhs '/\.embuild/' "${out_build}/CMakeFiles" 2>/dev/null \
                | grep -vF "${CARGO_WORKSPACE_DIR}/.embuild" | grep -q '/\.embuild/'; then
            contaminated="stale .embuild reference in CMakeFiles"
        fi
        if [ -n "${contaminated}" ]; then
            warn "stale esp-idf-sys build: ${out_build}"
            warn "  reason: ${contaminated} — not this workspace"
            warn "  removing the contaminated build dir (full IDF rebuild follows, ~10 min)"
            rm -rf "$(dirname "$(dirname "$(dirname "$(dirname "${out_build}")")")")"
        fi
    done
fi

run env RUSTUP_TOOLCHAIN="${TOOLCHAIN}" ESP_IDF_SDKCONFIG="${SDKCONFIG}" \
    cargo build --target "${TRIPLE}" "${RELEASE_ARGS[@]}" "${FEATURE_ARGS[@]}"

# Record the config only after a successful build. If cargo fails, leave the
# stamp unchanged so the next attempt still invalidates caches whose
# provenance is not known to match this sdkconfig.
if [ "${DRY_RUN}" -eq 0 ]; then
    mkdir -p "${TARGET_DIR}"
    cp "${SDKCONFIG}" "${STAMP}"
fi

# esptool cannot flash a raw cargo ELF ("will not fit in flash") — convert first.
run esptool --chip "${CHIP}" elf2image -o "${IMAGE}" "${ELF}"

if [ -n "${FLASH_PORT}" ]; then
    # The OTA partition table rides along with the app (Codex reviews on
    # #67/#73): a board still carrying a factory/single-app table would
    # otherwise lack the ota slots (and the c3 coredump partition) that this
    # build assumes. Idempotent — safe to reflash every time.
    PART_TABLE="${TARGET_DIR}/${TRIPLE}/${PROFILE}/build/partition-table.bin"
    # Codex review on #82: writing ota_0 while otadata still points at
    # ota_1 leaves the bootloader on the OLD image after a "successful"
    # flash. Erasing otadata resets boot selection to the first valid
    # app partition. Safe ONLY because PART_TABLE (our OTA table, where
    # 0x30000 is otadata) is flashed in the same invocation — under the
    # legacy single-app table that offset IS the factory app.
    OTADATA_OFFSET="0x30000"
    run esptool --chip "${CHIP}" -p "${FLASH_PORT}" --baud "${FLASH_BAUD}" \
        write-flash 0x8000 "${PART_TABLE}" "${FLASH_OFFSET}" "${IMAGE}"
    run esptool --chip "${CHIP}" -p "${FLASH_PORT}" --baud "${FLASH_BAUD}" \
        erase-region "${OTADATA_OFFSET}" 0x2000
    if [ "${CHIP}" = "esp32" ]; then
        warn "FTDI DTR/RTS wedge: physically replug the M5Stack board before expecting"
        warn "serial communication to work again (see flash_and_test.sh header)."
    fi
fi

info "done: ${IMAGE}"
