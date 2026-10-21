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
Usage: $0 <c3|c3-ccid|c3-ccid-ble|c3-ccid-ble-diag|m5stick|m5stick-plus-lcd|m5stick-ble|m5stick-spp|m5atom> [--flash <port>] [--dry-run]

Boards:
  c3           ESP32-C3 nucula, PN7160 bring-up (nightly toolchain, debug build,
               sdkconfig.full, ota_0 slot @ 0x40000, baud 460800)
  c3-ccid      ESP32-C3 nucula, PN7160 CCID over USB-CDC (as c3; the full
               reader firmware — flash + host pcscd via libccidtwin)
  c3-ccid-ble  as c3-ccid + BLE debug console (issue #66): CCID stays on the
               USB-CDC wire, logs ride NUS GATT notifications
               (sdkconfig-c3-ble.full, +ble feature). Capture:
               tests/hardware/ble/ble_log_capture.py
  m5stick      ESP32 M5StickC, MFRC522 CCID (esp toolchain, release,
               sdkconfig-xtensa.full, ota_0 slot @ 0x40000, baud 115200)
  m5stick-plus-lcd  as m5stick + the ST7789 status screen — M5StickC-**Plus**
               ONLY (#275: the original StickC's ST7735S 80x160 panel is
               mis-driven by the Plus geometry; per-model selection is the
               tracked follow-up)
  m5stick-ble  as m5stick + BLE debug console (issue #66): logs ride NUS GATT
               notifications (sdkconfig-xtensa-ble.full, +ble feature),
               UART0 carries CCID only. Capture:
               tests/hardware/ble/ble_log_capture.py
  m5stick-spp  as m5stick + pcscd over Bluetooth Classic SPP: RFCOMM replaces
               UART0 as the GemPC CCID wire, console/logs move to UART0
               (sdkconfig-xtensa-spp.full, +spp feature). Host side:
               rfcomm watch -> /dev/rfcomm0 + reader.conf.rfcomm
               (README "pcscd over Bluetooth SPP"). Classic BT = ESP32 only.
  m5atom       ESP32 M5Stack Atom, MFRC522 CCID (as m5stick, default features)


Options:
  --flash <port>  esptool write-flash after a successful build
  --dry-run       print the commands without executing anything
  -h, --help      show this help

Env:
  CARGO_TARGET_DIR             honored; defaults to ~/.cargo-target
  NUCULA_WIFI_SSID/_PASS       passed through (baked at build time)
  OTA_TOKEN                    32 chars — arms the authenticated OTA2
                               push protocol (ota_header); unset builds
                               keep the OTA1 (unauthenticated) form
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
        TOOLCHAIN="esp"  # pinned esp fork v1.98.1.0 (espup -v). NOT v1.99.0.0: its rust-src/libc pairing breaks -Zbuild-std on espidf (libc AT_FDCWD, rustix#1652 / rust-lang#160170). Retires the dated upstream nightly — mutable alias is a documented residual (Codex #150): espup owns this name; a version-check here duplicates board.py's documented decision (see its 'deliberately not enforced' note)
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
        TOOLCHAIN="esp"  # pinned esp fork v1.98.1.0 (espup -v). NOT v1.99.0.0: its rust-src/libc pairing breaks -Zbuild-std on espidf (libc AT_FDCWD, rustix#1652 / rust-lang#160170). Retires the dated upstream nightly
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig.full"
        TRIPLE="riscv32imc-esp-espidf"
        PROFILE="debug"
        RELEASE_ARGS=()
        FEATURE_ARGS=(--no-default-features --features pn7160-ccid)
        CHIP="esp32c3"
        FLASH_OFFSET="0x40000"
        FLASH_BAUD="460800"
        ;;
    c3-ccid-ble)
        # nucula + BLE debug console: CCID stays on the USB-CDC wire, logs
        # ride BLE GATT notifications (issue #66). Built through build.sh so
        # the sdkconfig stamp forces the cmake cache to pick up
        # sdkconfig-c3-ble.full (the 2026-10-09 ad-hoc build risked the
        # documented stale-cache trap — see AGENTS.md ESP_IDF_SDKCONFIG note).
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig-c3-ble.full"
        TRIPLE="riscv32imc-esp-espidf"
        PROFILE="debug"
        RELEASE_ARGS=()
        FEATURE_ARGS=(--no-default-features --features pn7160-ccid,ble)
        CHIP="esp32c3"
        FLASH_OFFSET="0x40000"
        FLASH_BAUD="460800"
        ;;
    c3-ccid-ble-diag)
        # #165 DIAGNOSTIC build (NEVER a serving image): c3-ccid-ble with
        # C-level BT stack logs unsilenced + raised to DEBUG
        # (sdkconfig-c3-ble-diag.full + the ble-diag feature) — the
        # on-device capture of what the controller really returns when
        # start_advertising runs.
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig-c3-ble-diag.full"
        TRIPLE="riscv32imc-esp-espidf"
        PROFILE="debug"
        RELEASE_ARGS=()
        FEATURE_ARGS=(--no-default-features --features pn7160-ccid,ble-diag)
        CHIP="esp32c3"
        FLASH_OFFSET="0x40000"
        FLASH_BAUD="460800"
        ;;
    wifi-ccid)
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig.full"
        TRIPLE="riscv32imc-esp-espidf"
        PROFILE="debug"
        RELEASE_ARGS=()
        FEATURE_ARGS=(--no-default-features --features "${BOARD},backend-pn7160,board-nucula,bench-net")
        CHIP="esp32c3"
        FLASH_OFFSET="0x40000"
        FLASH_BAUD="460800"
        ;;
    wifi-vpcd)
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig.full"
        TRIPLE="riscv32imc-esp-espidf"
        PROFILE="debug"
        RELEASE_ARGS=()
        FEATURE_ARGS=(--no-default-features --features "${BOARD},backend-pn7160,board-nucula,bench-net")
        CHIP="esp32c3"
        FLASH_OFFSET="0x40000"
        FLASH_BAUD="460800"
        ;;
    m5stick-vpcd)
        # m5stick + MFRC522 (Grove I2C) running the vpcd reversed-connection
        # firmware — the computer-USB-power proof: no wall adapter involved.
        # Console returns to UART0 (the USB bridge): the CCID serial wire is
        # unused by vpcd, and netlog keeps carrying the same lines over UDP.
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig-xtensa-vpcd.full"
        TRIPLE="xtensa-esp32-espidf"
        PROFILE="release"
        RELEASE_ARGS=(--release)
        FEATURE_ARGS=(--no-default-features --features wifi-vpcd,backend-mfrc522,board-m5stick,bench-net)
        CHIP="esp32"
        FLASH_OFFSET="0x40000"  # ota_0 slot per partitions-ota.csv (Codex #79 P1)
        FLASH_BAUD="115200"
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
    m5stick-plus-lcd)
        # m5stick + MFRC522 CCID + the ST7789 status screen (bolty-rs
        # display.rs port): AXP192 backlight, FWID/variant/battery/uptime.
        # GPIO27 (backlight) means no Atom LED matrix on this variant.
        # PLUS-ONLY (issue #275): the pinned 135x240/ST7789 geometry
        # mis-drives an original M5StickC's ST7735S panel — do not flash
        # this variant onto non-Plus hardware.
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig-xtensa.full"
        TRIPLE="xtensa-esp32-espidf"
        PROFILE="release"
        RELEASE_ARGS=(--release)
        FEATURE_ARGS=(--no-default-features --features backend-mfrc522,board-m5stick,lcd)
        CHIP="esp32"
        FLASH_OFFSET="0x40000"
        FLASH_BAUD="115200"
        ;;
    m5stick-ble)
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig-xtensa-ble.full"
        TRIPLE="xtensa-esp32-espidf"
        PROFILE="release"
        RELEASE_ARGS=(--release)
        FEATURE_ARGS=(--no-default-features --features backend-mfrc522,board-m5stick,ble)
        CHIP="esp32"
        FLASH_OFFSET="0x40000"  # ota_0 slot per partitions-ota.csv (Codex #79 P1)
        FLASH_BAUD="115200"
        ;;
    m5stick-spp)
        TOOLCHAIN="esp"
        SDKCONFIG="${SCRIPT_DIR}/sdkconfig-xtensa-spp.full"
        TRIPLE="xtensa-esp32-espidf"
        PROFILE="release"
        RELEASE_ARGS=(--release)
        FEATURE_ARGS=(--no-default-features --features backend-mfrc522,board-m5stick,spp)
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
        echo "error: unknown board: ${BOARD} (expected c3, c3-ccid, c3-ccid-ble, c3-ccid-ble-diag, m5stick, m5stick-plus-lcd, m5stick-ble, m5stick-spp, or m5atom)" >&2
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
# #223: arm the authenticated OTA2 push protocol when the bench passes a
# token; a wrong-length token would arm nothing (ota_header panics at
# runtime) — refuse it here, at the build, loudly.
if [ -n "${OTA_TOKEN:-}" ]; then
    # BYTE length, not ${#...} character length: compiled_token()
    # compares 32 bytes on the wire (Codex review on #225) — under a
    # UTF-8 locale a 32-multibyte-char token would pass a char count
    # and arm a firmware that panics at the first push.
    OTA_TOKEN_BYTES="$(printf '%s' "${OTA_TOKEN}" | wc -c)"
    if [ "${OTA_TOKEN_BYTES}" -eq 32 ]; then
        info "OTA_TOKEN: set (32 bytes — OTA2 authenticated pushes armed)"
    else
        echo "error: OTA_TOKEN must be exactly 32 chars (ota_header::TOKEN_LEN)" >&2
        exit 1
    fi
else
    info "OTA_TOKEN: unset — OTA1 (unauthenticated) push protocol"
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
# sdkconfig. The stamp records the last-applied sdkconfig — when it differs
# (or when caches exist but no stamp does — first run against a shared
# target dir predating this scheme), the caches are dropped before cargo
# runs so the new config is real. The stamp is keyed per (triple, profile)
# — NOT per board — because boards sharing a target triple+profile (c3 vs
# c3-ccid vs c3-ccid-ble, m5stick vs m5atom) share one esp-idf-sys cmake
# cache; a per-board stamp would let e.g. a non-BT cache silently serve a
# `-ble` build after switching boards (issue #66 pairing rule).
#
# The stamp ALSO records the manifest's esp_idf_version (Codex #85 P1,
# issue #112): cmake caches the IDF checkout identity too, so bumping
# [package.metadata.esp-idf-sys] esp_idf_version in Cargo.toml with an
# unchanged sdkconfig needs the same cache drop — previously that case
# silently kept building the OLD ESP-IDF until a manual cargo clean.
IDF_VER="$(sed -n 's/^[[:space:]]*esp_idf_version[[:space:]]*=[[:space:]]*"\([^"]*\)"/\1/p' "${SCRIPT_DIR}/Cargo.toml" | head -n1)"
if [ -z "${IDF_VER}" ]; then
    warn "could not parse esp_idf_version from Cargo.toml — stamping as 'unknown'"
    IDF_VER="unknown"
fi
STAMP="${TARGET_DIR}/sdkconfig.${TRIPLE}.${PROFILE}.stamp"
# Stamp body = IDF version header + the applied sdkconfig content.
stamp_body() {
    printf '# esp_idf_version: %s\n' "${IDF_VER}"
    cat "${SDKCONFIG}"
}
if [ "${DRY_RUN}" -eq 0 ]; then
    STALE=0
    if [ ! -f "${STAMP}" ]; then
        # No stamp + existing esp-idf-sys caches = unknown provenance.
        for cache in "${TARGET_DIR}/${TRIPLE}"/*/build/esp-idf-sys*/out/build/CMakeCache.txt; do
            [ -f "${cache}" ] && STALE=1
        done
    elif ! stamp_body | cmp -s - "${STAMP}"; then
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

# Both chips use the esp-idf-sys GLOB injection: partitions-ota.csv is
# copied into the embuild out dir BEFORE cmake configure (robust against
# fingerprint changes recreating the out dir — the old post-hoc cp dance
# lost the file exactly there, bench 2026-10-09).
export ESP_IDF_GLOB_PARTCSV_BASE="${SCRIPT_DIR}"
export ESP_IDF_GLOB_PARTCSV_1="/partitions-ota.csv"

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
    stamp_body > "${STAMP}"
fi

# esptool cannot flash a raw cargo ELF ("will not fit in flash") — convert first.
# --flash-mode dio is load-bearing (#309): esptool >=5 defaults the image
# header to QIO and a QIO-header app silently boot-loops every DIO board
# this repo ships (all sdkconfigs mandate dio). The header guard below
# fails loud if any future toolchain default changes.
run esptool --chip "${CHIP}" elf2image --flash-mode dio -o "${IMAGE}" "${ELF}"
if [ "${DRY_RUN}" -eq 0 ]; then
    img_mode="$(od -An -tx1 -j2 -N1 "${IMAGE}" | tr -d ' \n')"
    if [ "${img_mode}" != "02" ]; then
        echo "error: ${IMAGE} flash-mode byte is 0x${img_mode} (expected 02/dio) — refusing (issue #309)" >&2
        exit 1
    fi
fi

if [ -n "${FLASH_PORT}" ]; then
    # The OTA partition table rides along with the app (Codex reviews on
    # #67/#73): a board still carrying a factory/single-app table would
    # otherwise lack the ota slots (and the c3 coredump partition) that this
    # build assumes. Idempotent — safe to reflash every time.
    # Location: inside the esp-idf-sys out dir (esp-idf-sys-<hash>/out/
    # build/partition_table/) — the hash dir changes with fingerprints, so
    # resolve by newest match; fall back to the legacy flat path.
    PART_TABLE="$(ls -t "${TARGET_DIR}/${TRIPLE}"/${PROFILE}/build/esp-idf-sys*/out/build/partition_table/partition-table.bin 2>/dev/null | head -1)"
    if [ -z "${PART_TABLE}" ]; then
        PART_TABLE="${TARGET_DIR}/${TRIPLE}/${PROFILE}/build/partition-table.bin"
    fi
    # Codex review on #82: writing ota_0 while otadata still points at
    # ota_1 leaves the bootloader on the OLD image after a "successful"
    # flash. Erasing otadata resets boot selection to the first valid
    # app partition. Safe ONLY because PART_TABLE (our OTA table, where
    # 0x30000 is otadata) is flashed in the same invocation — under the
    # legacy single-app table that offset IS the factory app.
    OTADATA_OFFSET="0x30000"
    # Wedge discipline (2026-10-19 m5stick session, #278/#299): every
    # flash goes through esp32_flash.py — pcscd socket+service quiesce
    # under a HELD bench flock, baud capped to the exporter-configured
    # speed, writes-before-erases ordering (descending offsets: the
    # partition table lands LAST), the USBDEVFS_RESET/ftdi-rebind
    # ladder, FTDI latency_timer restore, and the boot signature check.
    # Never a hand-rolled esptool sequence on a bench reader port.
    ESP_FLASH_TOOL="${SCRIPT_DIR}/../../tests/hardware/labgrid/esp32_flash.py"
    ESP_FLASH_ARGS=(--port "${FLASH_PORT}" --chip "${CHIP}" --baud "${FLASH_BAUD}"
        --image 0x8000="${PART_TABLE}" --image "${FLASH_OFFSET}=${IMAGE}"
        --erase "${OTADATA_OFFSET}=0x2000")
    if [ -f "${SCRIPT_DIR}/../../tests/hardware/labgrid/exporter-ai-legion-nfc.yaml" ]; then
        ESP_FLASH_ARGS+=(--exporter-cfg "${SCRIPT_DIR}/../../tests/hardware/labgrid/exporter-ai-legion-nfc.yaml")
    fi
    # #335: sudo — the tool's pcscd stop/start need service rights; an
    # unsudoed stop fails and the tool correctly refuses to flash (same
    # elevation flash_and_test.sh:111-114 already uses for this tool —
    # this was the remaining unsudoed caller).
    run sudo python3 "${ESP_FLASH_TOOL}" "${ESP_FLASH_ARGS[@]}"
    if [ "${CHIP}" = "esp32" ]; then
        warn "FTDI DTR/RTS wedge: physically replug the M5Stack board before expecting"
        warn "serial communication to work again (see flash_and_test.sh header)."
    fi
fi

info "done: ${IMAGE}"
