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
# MFRC522 backend (M5Stack Atom, default)
cargo build --release --target xtensa-esp32-espidf --features backend-mfrc522

# PN532 backend (original hardware)
cargo build --release --target xtensa-esp32-espidf --features backend-pn532
```

If neither flag is specified, `backend-mfrc522` is the default.

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
cargo build --release --target xtensa-esp32-espidf --features backend-pn532
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

## Host setup

### 1. Install pcscd and drivers

```bash
sudo apt install pcscd libccid pcsc-tools
```

### 2. Install reader config

Copy the provided `reader.conf` to the pcscd config directory:

```bash
sudo cp esp32-ccid/reader.conf /etc/reader.conf.d/GemPCTwin.conf
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
cargo test
```

60 tests covering serial framing, CCID message parsing, NFC logic, and MFRC522 transceiver bridging.

## Known limitations

- **NFC only** — does not support contact smart cards
- **Synthetic ATR** — the ATR returned to the host is generated from NFC chip ATS, not from an actual contact card
- **No SAM** — the PN532's Secure Access Module is not used
- **Single slot** — only one card at a time
- **Short APDU only** — extended APDU not supported
- **115200 baud fixed** — no auto-baudrate negotiation
- **LED stub** — M5Stack Atom LED matrix status is logged but not physically driven (pending ws2812 driver integration)
