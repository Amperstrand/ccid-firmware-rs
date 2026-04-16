# esp32-ccid

ESP32 firmware that emulates a **GemPC Twin** serial CCID smart card reader using a PN532 NFC module. When connected via USB-UART, `pcscd` with `libccidtwin` recognizes it as a standard PC/SC reader — tap an NFC card and `pcsc_scan` sees it.

## How it works

```
Host (Linux)                    ESP32                          NFC Card
─────────────              ──────────────                  ──────────
pcscd/libccidtwin.so  ──UART 115200 8N2──>  serial_framing  ──SPI──>  PN532
     ↕ CCID commands        (GemPC Twin protocol)           (ISO 14443A)
```

The ESP32 speaks the **GemPC Twin serial protocol** (SYNC + CTRL + CCID message + LRC framing) over UART. `pcscd` loads `libccidtwin.so` which drives the serial port. The ESP32 translates CCID smart card commands into PN532 SPI commands to communicate with NFC cards via ISO 14443A.

## Hardware requirements

| Component | Notes |
|-----------|-------|
| ESP32-WROOM-32 | DevKit or bare module |
| PN532 NFC module | SPI variant (not I2C/UART) |
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

## Build

Requires the [ESP-IDF toolchain](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/get-started/) and the Rust `xtensa-esp32-espidf` target.

```bash
# Install the target (one-time)
rustup target add xtensa-esp32-espidf

# Build
cargo build --release --target xtensa-esp32-espidf
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

1. Flash the ESP32 firmware
2. Connect the ESP32 via USB-UART (appears as `/dev/ttyUSB0`)
3. Run `./setup.sh` (or manually install the reader config and restart pcscd)
4. Run `pcsc_scan` — the GemPC Twin reader should appear
5. Tap an NFC card on the PN532 module — the ATR updates

## Architecture

| Module | Responsibility |
|--------|---------------|
| `main.rs` | Entry point, UART0 (115200 8N2) and SPI2 initialization, main loop |
| `serial_framing.rs` | GemPC Twin serial framing: SYNC/CTRL/CCID/LRC, echo handling |
| `ccid_handler.rs` | CCID command dispatch (IccPowerOn, XfrBlock, etc.), init handshake |
| `ccid_types.rs` | CCID message structs, RDR_to_PC slot status, data rates |
| `pn532_driver.rs` | PN532 SPI driver: SAM configuration, InListPassiveTarget, InDataExchange |
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

51 tests covering serial framing, CCID message parsing, and NFC logic.

## Known limitations

- **NFC only** — does not support contact smart cards
- **Synthetic ATR** — the ATR returned to the host is generated from PN532 ATS, not from an actual contact card
- **No SAM** — the PN532's Secure Access Module is not used
- **Single slot** — only one card at a time
- **Short APDU only** — extended APDU not supported
- **115200 baud fixed** — no auto-baudrate negotiation
