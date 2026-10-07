# esp32-ccid

ESP32 firmware that emulates a **GemPC Twin** serial CCID smart card reader. Supports two NFC backends selected at build time via feature flags:

- **MFRC522** (default) — I2C-connected, targeting the M5Stack Atom Matrix
- **PN532** (original) — SPI-connected, targeting generic ESP32 dev boards

When connected via USB-UART, `pcscd` with `libccidtwin` recognizes either configuration as a standard PC/SC reader. Tap an NFC card and `pcsc_scan` sees it.

## How it works

Both backends speak the same **GemPC Twin serial protocol** over UART. The only difference is which NFC chip sits behind the ESP32.

### PN532 path (SPI)

```
Host (Linux)                    ESP32                          NFC Card
─────────────              ──────────────                  ──────────
pcscd/libccidtwin.so  ──UART 115200 8N2──>  serial_framing  ──SPI──>  PN532
     ↕ CCID commands        (GemPC Twin protocol)           (ISO 14443A)
```

### MFRC522 path (M5Stack Atom, I2C)

```
Host (Linux)                    M5Stack Atom                   NFC Card
─────────────              ──────────────                  ──────────
pcscd/libccidtwin.so  ──UART 115200 8N2──>  serial_framing  ──I2C──>  MFRC522
     ↕ CCID commands        (GemPC Twin protocol)           (ISO 14443A)
```

The ESP32 translates CCID smart card commands into NFC chip commands (SPI for PN532, I2C for MFRC522) to communicate with NFC cards via ISO 14443A.

## Feature flags

Select which NFC backend to compile against:

```bash
# MFRC522 backend, M5Stack Atom Grove pinout (SDA=26/SCL=32, default)
cargo build --release --target xtensa-esp32-espidf

# MFRC522 backend, M5Stick Grove pinout (SDA=32/SCL=33)
cargo build --release --target xtensa-esp32-espidf --no-default-features --features backend-mfrc522,board-m5stick

# PN532 backend (original hardware, SPI — board feature not used)
cargo build --release --target xtensa-esp32-espidf --no-default-features --features backend-pn532
```

Backends are mutually exclusive and enforced by `compile_error!`: selecting a
non-default backend without `--no-default-features` fails the build (defaults
enable `backend-mfrc522` — it would otherwise silently win).

## Hardware requirements

| Component | Notes |
|-----------|-------|
| ESP32-WROOM-32 | DevKit, bare module, or M5Stack Atom Matrix |
| PN532 NFC module | SPI variant (not I2C/UART), for PN532 backend |
| MFRC522 NFC module | I2C variant, for MFRC522 backend |
| M5Stack Atom Matrix | ESP32 + 5x5 WS2812 LEDs + Grove I2C port, for MFRC522 backend |
| CP2102 USB-UART | Shows as `/dev/ttyUSB0` on Linux |
| NFC card | ISO 14443A (MIFARE, NTAG, FeliCa, etc.) |

### SPI wiring (ESP32 → PN532)

| Signal | ESP32 GPIO | PN532 Pin |
|--------|-----------|-----------|
| SCK    | GPIO19    | SCK       |
| MISO   | GPIO18    | MISO      |
| MOSI   | GPIO17    | MOSI      |
| CS     | GPIO25    | NSS       |
| RST    | GPIO26    | RST       |
| IRQ    | GPIO16    | IRQ       |

Connect VCC (3.3V) and GND between ESP32 and PN532. The PN532 must be powered from 3.3V.

### M5Stack Atom + MFRC522 wiring

The M5Stack Atom Matrix has a Grove connector wired to I2C1 and an onboard WS2812C LED matrix. Connect an MFRC522 breakout to the Grove I2C port.

#### I2C connection (Grove port → MFRC522)

| Signal | M5Stack Atom GPIO | MFRC522 Pin | Notes |
|--------|------------------|-------------|-------|
| SDA    | GPIO26           | SDA         | I2C1 data |
| SCL    | GPIO32           | SCL         | I2C1 clock |
| VCC    | 3.3V             | VCC         | |
| GND    | GND              | GND         | |

I2C bus runs at 400 kHz. The MFRC522 I2C address is `0x28`.

#### LED matrix

| Signal | GPIO | Notes |
|--------|------|-------|
| WS2812C data | GPIO27 | 5×5 RGB LED matrix, driven via ESP32 RMT peripheral |

The LED matrix is driven using the ESP32's built-in RMT peripheral (no external crates). Brightness is capped at 15/255 (M5Stack recommends ≤20 to avoid LED/acrylic damage). Each state displays a distinct pattern on the 5×5 grid for at-a-glance diagnostics.

#### LED status patterns

| State | Pattern | Color | Meaning |
|-------|---------|-------|---------|
| Init | Center pixel | Amber | Hardware initializing |
| Ready | Center pixel | Green | Initialized, waiting for card |
| Card Present | Border ring (12 LEDs) | Blue | Card detected on NFC field |
| TxRx | Center pixel | Yellow | CCID command in progress (flashes) |
| Error | X pattern (both diagonals) | Red | Initialization or communication error |
| Off | All black | — | LEDs off |

## Build

Requires the [ESP-IDF toolchain](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/get-started/) and the Rust `xtensa-esp32-espidf` target.

```bash
# Install the target (one-time)
rustup target add xtensa-esp32-espidf

# Build (MFRC522 backend, default)
cargo build --release --target xtensa-esp32-espidf

# Build (PN532 backend)
cargo build --release --target xtensa-esp32-espidf --no-default-features --features backend-pn532
```

The firmware binary will be at `target/xtensa-esp32-espidf/release/esp32-ccid`.

## Flash

Connect the ESP32 via USB and flash with `espflash`:

```bash
espflash flash --monitor target/xtensa-esp32-espidf/release/esp32-ccid
```

Or via cargo:

```bash
cargo espflash flash --monitor target/xtensa-esp32-espidf/release/esp32-ccid
```

## ESP-IDF Rust toolchain and bring-up builds (verified on ai-legion, 2026-10)

The esp-idf targets (`riscv32imc-esp-espidf`, `xtensa-esp32-espidf`) have **no
precompiled std** — esp-idf-sys builds std from source via `-Zbuild-std`, so
`rustup target add` is not applicable. Per-target toolchains:

- **ESP32-C3 / Nucula (RISC-V):** `RUSTUP_TOOLCHAIN=nightly` with `rustup component add rust-src`
- **ESP32 / M5Stick (Xtensa):** `RUSTUP_TOOLCHAIN=esp` (installed by `espup install --targets esp32,esp32c3`, which also writes `~/export-esp.sh`)

```bash
source ~/export-esp.sh   # LIBCLANG_PATH (esp-clang) + Xtensa GCC
cd firmware/esp32-ccid

# C3 / Nucula PN7160 bring-up
# ESP_IDF_GLOB_* injects partitions-ota.csv into the embuild project dir
# before cmake configure (or just use build.sh, which sets all of this)
RUSTUP_TOOLCHAIN=nightly \
ESP_IDF_SDKCONFIG=$PWD/sdkconfig.full \
ESP_IDF_GLOB_PARTCSV_BASE=$PWD \
ESP_IDF_GLOB_PARTCSV_1="/partitions-ota.csv" \
NUCULA_WIFI_SSID="<ssid>" NUCULA_WIFI_PASS="<pass>" \
cargo build --target riscv32imc-esp-espidf \
  --no-default-features --features pn7160-bringup,pn7160-verdict-b

# M5Stick / MFRC522 CCID
RUSTUP_TOOLCHAIN=esp \
ESP_IDF_SDKCONFIG=$PWD/sdkconfig-xtensa.full \
NUCULA_WIFI_SSID="<ssid>" NUCULA_WIFI_PASS="<pass>" \
cargo build --release --target xtensa-esp32-espidf \
  --no-default-features --features backend-mfrc522,board-m5stick
```

Gotchas verified the hard way:

- **`ESP_IDF_SDKCONFIG`, not `SDKCONFIG`** — esp-idf-sys 0.37 only reads the
  `ESP_IDF_*`-prefixed variables. With the bare name the build silently falls
  back to ESP-IDF defaults (no 32 KB main-task stack, task WDT on).
- **Partition CSV resolution (C3)** — `sdkconfig.full` references
  `partitions-ota.csv` by its real name and the `ESP_IDF_GLOB_PARTCSV_*`
  variables above inject it into the embuild project dir before cmake
  configure. The Xtensa flow is different: `sdkconfig-xtensa.full` expects a
  renamed `partitions.csv` — `build.sh` copies it (target-dir root + existing
  esp-idf-sys out dirs) for the m5stick/m5atom boards; for a bare build run
  `cp partitions-ota.csv <target-dir>/partitions.csv`.
- **esptool needs a converted image** — the extension-less cargo ELF is written
  raw ("will not fit in flash"); run `esptool --chip esp32c3 elf2image` first.
- **WiFi credentials are baked by `option_env!`** — tracked as compilation
  inputs via `build.rs` `rerun-if-env-changed` (`00b36a0`); a plain rebuild
  picks up changed credentials. (On older checkouts without that fix, touch
  the main that reads them before a credential change.)

Flash (esptool; C3 console is the same USB-Serial/JTAG CDC port):

```bash
# C3 / Nucula — ota_0 slot of the OTA table, 4 MB flash
esptool --chip esp32c3 elf2image -o /tmp/c3.bin <target-dir>/riscv32imc-esp-espidf/debug/esp32-ccid
esptool --chip esp32c3 -p /dev/serial/by-id/<espressif-jtag-port> --baud 460800 write-flash 0x40000 /tmp/c3.bin

# M5Stick — factory slot of the on-device table (nvs@0x9000, factory@0x30000/3904K)
esptool --chip esp32 elf2image -o /tmp/m5.bin <target-dir>/xtensa-esp32-espidf/release/esp32-ccid
esptool --chip esp32 -p /dev/serial/by-id/<m5stick-port> --baud 115200 write-flash 0x30000 /tmp/m5.bin
```

On a failed station connect the firmware now scans and logs every visible AP
(SSID/channel/RSSI/auth) — distinguishes "target SSID out of range from this
board" from association/auth failures without host-side tooling.

## Host setup

### 1. Install pcscd and drivers

```bash
sudo apt install pcscd libccid pcsc-tools
```

### 2. Install reader config

Copy the provided `reader.conf` to the pcscd config directory:

```bash
sudo cp firmware/esp32-ccid/reader.conf /etc/reader.conf.d/GemPCTwin.conf
```

This tells `pcscd` to use `libccidtwin.so` for `/dev/ttyUSB0`.

### 3. Restart pcscd

```bash
sudo systemctl restart pcscd
```

### 4. Verify

```bash
pcsc_scan
```

You should see the GemPC Twin reader listed with an ATR.

## Quick start with setup.sh

The included `setup.sh` automates the host setup:

```bash
# Check prerequisites, install config, restart pcscd
./setup.sh

# Also flash the firmware
./setup.sh --flash

# Flash and verify with pcsc_scan
./setup.sh --flash --verify
```

## Usage

1. Flash the ESP32 firmware (choose backend via feature flags)
2. Connect the ESP32 via USB-UART (appears as `/dev/ttyUSB0`)
3. Run `./setup.sh` (or manually install the reader config and restart pcscd)
4. Run `pcsc_scan` — the GemPC Twin reader should appear
5. Tap an NFC card on the NFC module — the ATR updates

## Architecture

| Module | Responsibility |
|--------|---------------|
| `main.rs` | Entry point, UART0 (115200 8N2) and peripheral initialization, main loop |
| `serial_framing.rs` | GemPC Twin serial framing: SYNC/CTRL/CCID/LRC, echo handling |
| `ccid_handler.rs` | CCID command dispatch (IccPowerOn, XfrBlock, etc.), init handshake |
| `ccid_types.rs` | CCID message structs, RDR_to_PC slot status, data rates |
| `pn532_driver.rs` | PN532 SPI driver: SAM configuration, InListPassiveTarget, InDataExchange (PN532 backend) |
| `mfrc522_driver.rs` | MFRC522 NFC driver: ISO 14443-4 APDU via iso14443 crate (MFRC522 backend) |
| `pn7160_driver.rs` | PN7160 NCI driver over `pn7160-nci` (nucula backend) |
| `pn7160_i2c.rs` | PN7160 I2C + VEN/IRQ transport, verdict-selectable bring-up constructors (target-only) |
| `pn7160_bringup.rs` | PN7160 bring-up mains: NCI init ladder + card heartbeat (target-only) |
| `pn7160_ccid.rs` | nucula CCID main: serves CCID over the USB-Serial/JTAG CDC port (target-only) |
| `ccid_serial_server.rs` | Host-testable GemPC serial CCID serving core: echo, framed response, interval-gated card polling (shared by the USB-CDC main) |
| `wifi.rs`/`ota.rs`/`netlog.rs` | WiFi station, OTA update, UDP logging for serial-free bring-up (target-only) |
| `mfrc522_transceiver.rs` | PcdTransceiver bridge between mfrc522 crate and iso14443 (MFRC522 backend) |
| `led.rs` | M5Stack Atom LED status display (WS2812 RMT driver, 5×5 grid patterns) |
| `nfc.rs` | NFC card management: card detection, ATR generation, APDU relay |
| `lib.rs` | Shared types and host-testable abstractions |

### Init handshake

On connection, `libccidtwin` sends two `CmdEscape` sequences:

1. `CmdEscape(0x02)` — firmware version query
2. `CmdEscape(0x01, 0x01, 0x01)` — enable sync notifications

The firmware responds to both, completing the GemPC Twin identification.

### Serial framing

All UART traffic uses the GemPC Twin framing format:

```
[0x03] [CTRL] [10-byte CCID header] [data...] [LRC]
```

- `0x03` — SYNC byte
- `CTRL` — `0x06` (ACK) or `0x15` (NAK)
- LRC — XOR of all preceding bytes (including SYNC and CTRL)

## Testing

Host-side unit tests (no hardware required):

```bash
cargo test --target x86_64-unknown-linux-gnu
```

80 tests covering serial framing, CCID message parsing, NFC logic, LED pattern logic, MFRC522 transceiver bridging, and the GemPC serial CCID serving core (echo/response framing, poll gating).

## Known limitations

- **NFC only** — does not support contact smart cards
- **Synthetic ATR** — the ATR returned to the host is generated from NFC chip ATS, not from an actual contact card
- **No SAM** — the PN532's Secure Access Module is not used
- **Single slot** — only one card at a time
- **Short APDU only** — extended APDU not supported
- **115200 baud fixed** — no auto-baudrate negotiation
- **Target-specific LED driver** — the M5Stack Atom LED matrix is driven on Xtensa hardware builds; host tests use a stubbed implementation so shared state and pattern logic stay testable without ESP32 hardware
