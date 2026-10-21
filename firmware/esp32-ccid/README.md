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
cargo build --release --target xtensa-esp32-espidf --features backend-mfrc522

# MFRC522 backend, M5Stick Grove pinout (SDA=32/SCL=33)
cargo build --release --target xtensa-esp32-espidf --features backend-mfrc522,board-m5stick

# PN532 backend (original hardware, SPI — board feature not used)
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

Connect the ESP32 via USB and flash with the wedge-discipline helper
(`espflash` is deliberately NOT used — its DTR/RTS toggling wedges the
FTDI bridge; see AGENTS "ESP32 serial wedge table"):

```bash
# the full helper: build + flash (boot-proven) + pcscd test
./flash_and_test.sh

# or any build.sh board with --flash (routes through esp32_flash.py)
./build.sh m5stick --flash /dev/serial/by-id/<port>
```

## ESP-IDF Rust toolchain and bring-up builds (verified on ai-legion, 2026-10)

The esp-idf targets (`riscv32imc-esp-espidf`, `xtensa-esp32-espidf`) have **no
precompiled std** — esp-idf-sys builds std from source via `-Z build-std`, so
`rustup target add` is not applicable. One-Rust-version policy (2026-10-12):
BOTH esp32 targets ride the **pinned esp fork `v1.98.1.0`** (`espup install
-c 14.2.0_20260121 -v 1.98.1.0 --targets esp32,esp32c3`) — the same Rust
version number as the stable pin, and the fork (a nightly-derived but frozen
vendor release) is the only compiler with xtensa espidf codegen anyway. The
dated upstream nightly is retired.

```bash
source ~/export-esp.sh   # LIBCLANG_PATH (esp-clang) + GCC
cd firmware/esp32-ccid

# C3 / Nucula PN7160 bring-up
# ESP_IDF_GLOB_* injects partitions-ota.csv into the embuild project dir
# before cmake configure (or just use build.sh, which sets all of this)
RUSTUP_TOOLCHAIN=esp \
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
  Always pass `--flash-mode dio` (#308): esptool ≥5 defaults the image header
  to QIO, which boot-loops the DIO-wired boards behind a silent ROM banner —
  `build.sh`/`flash_and_test.sh` assert the header byte (0x2) after converting.
- **WiFi credentials are baked by `option_env!`** — tracked as compilation
  inputs via `build.rs` `rerun-if-env-changed` (`00b36a0`); a plain rebuild
  picks up changed credentials. (On older checkouts without that fix, touch
  the main that reads them before a credential change.)

Flash (the same wedge-discipline helper applies on the bench — for a
manual C3/M5Stick flow, `esp32_flash.py` owns quiesce/ordering/ladder;
raw esptool sequences are bench-banned):

```bash
# C3 / Nucula — ota_0 slot of the OTA table, 4 MB flash
esptool --chip esp32c3 elf2image --flash-mode dio -o /tmp/c3.bin <target-dir>/riscv32imc-esp-espidf/debug/esp32-ccid
python3 ../../tests/hardware/labgrid/esp32_flash.py --chip esp32c3 \
  -p /dev/serial/by-id/<espressif-jtag-port> \
  --quiesce-unit nucula-ccid-broker \
  --image 0x8000=<target-dir>/riscv32imc-esp-espidf/debug/build/partition-table.bin \
  --image 0x40000=/tmp/c3.bin --erase 0x30000=0x2000

# M5Stick — OTA table, ota_0 slot
esptool --chip esp32 elf2image --flash-mode dio -o /tmp/m5.bin <target-dir>/xtensa-esp32-espidf/release/esp32-ccid
python3 ../../tests/hardware/labgrid/esp32_flash.py --chip esp32 \
  -p /dev/serial/by-id/<m5stick-port> --baud 115200 --boot-check wire \
  --image 0x8000=<target-dir>/xtensa-esp32-espidf/release/build/partition-table.bin \
  --image 0x40000=/tmp/m5.bin --erase 0x30000=0x2000
```

Flag notes (#332): the nucula CDC is permanently held by the standing
`nucula-ccid-broker` service — a c3 flash MUST pass
`--quiesce-unit nucula-ccid-broker` (the tool stops/starts it under the
held pcscd flock; `build.sh` passes it for every `esp32c3` board).
`sdkconfig-xtensa.full` builds are console-none
(`CONFIG_ESP_CONSOLE_NONE=y`): their proof-of-boot is `--boot-check wire`
(or `--console-none`, which auto-defaults an unspecified `--boot-check`
to the wire check) — the console signature can never appear.

On a failed station connect the firmware now scans and logs every visible AP
(SSID/channel/RSSI/auth) — distinguishes "target SSID out of range from this
board" from association/auth failures without host-side tooling.

## Dump-and-retrieve debugging (preferred)

For time-sensitive paths (NFC card I/O, CCID wire timing) prefer capturing
state post-mortem over live logging — no debug channel means no timing
perturbation and no wire contention:

```bash
python3 ../../tests/hardware/esp32_coredump.py trigger /dev/ttyACM0  # Escape 0xD1
sleep 6                                                              # coredump write + reboot
python3 ../../tests/hardware/esp32_coredump.py retrieve <firmware-ELF>
python3 ../../tests/hardware/esp32_coredump.py erase    /dev/ttyACM0  # when done
```

The firmware acks the escape, then panics with a marker reason so the
panic handler writes a flash coredump of the live state (crashes write
one automatically). See AGENTS.md "Crash Dumps & Snapshot Debugging"
for limits and the complementary channels (Escape 0xD0 counters,
`pn7160-actdiag`, GDB-over-JTAG).

## BLE debug builds (issue #66) — EXPERIMENTAL

`m5stick-ble` routes firmware logs to BLE GATT notifications (Nordic
UART Service lookalike) so the UART0 wire carries CCID frames only.
Parked as experimental: bench-central connects are flaky (host-side
BlueZ quirk), and the ESP32-C3 path is blocked by a controller quirk
(the `c3-ccid-ble` board builds, but advertising is rejected — see
AGENTS.md "BLE Debug Console" > C3 traps and issue #66). The code is
feature-gated and costs nothing in standard builds.

```bash
./build.sh m5stick-ble  --flash /dev/serial/by-id/<m5stick-port>
sudo rfkill unblock bluetooth && sudo hciconfig hci0 up
python3 ../../tests/hardware/ble/ble_log_capture.py
```

Notes:
- The `ble` feature requires a BT-enabled sdkconfig; a mismatched pair
  fails at compile time (deliberately loud).
- Under `ble` the main must never call `EspLogger::initialize_default()`
  — `BleConsole::init` installs the log sink (first installer wins).
- WiFi/netlog is compiled out under `ble` (modem is exclusive).
- BLE builds are bench-debug builds: HIL/labgrid verification stays on
  standard builds (FWID lands on the console there; in ble builds it
  goes to the BLE ring). See AGENTS.md "BLE Debug Console".

## pcscd over Bluetooth Classic SPP (`spp` feature)

The `spp` build swaps the transport: instead of UART0/FTDI, the GemPC
serial CCID server rides a **Bluetooth Classic SPP** (RFCOMM) connection.
SPP is a real serial line to libccidtwin — on the host, BlueZ's `rfcomm`
exposes the connection as `/dev/rfcomm0`, pcscd opens it with the stock
`libccidtwin.so`, and **no daemon and no custom host code** is needed.
The serving core (`ccid_serial_server` + `ccid_handler`) is reused
verbatim; only the byte pipe changes (`src/spp_server.rs`, REFERENCE:
esp-idf-svc `examples/bt_spp_acceptor.rs`).

```
Host (Linux)                       ESP32 (m5stick)                NFC Card
─────────────                 ────────────────────                ──────────
pcscd/libccidtwin.so           spp_server (GemPC Twin             MFRC522
  ↕ /dev/rfcomm0  ←rfcomm←    protocol over RFCOMM,   ──I2C──>   (ISO 14443A)
  (BlueZ SPP)                   BT Classic acceptor)
```

Firmware differences from the UART build:

- **UART0 is free** — console and Rust logs stay enabled there (the CCID
  wire is RFCOMM; console output cannot reach it).
- WiFi/OTA and the `ble` console are compiled out (one Bluetooth modem —
  `ble` + `spp` is a compile error).
- ESP32 silicon only (BR/EDR); requires `sdkconfig-xtensa-spp.full`
  (classic-only Bluedroid + SPP). A mismatched sdkconfig/feature pair
  fails to compile, same loud-pairing rule as `ble`.

Build + flash:

```bash
./build.sh m5stick-spp --flash /dev/serial/by-id/<m5stick-port>
# boot proof on the UART0 console (115200):
#   FWID esp32-ccid rev=<git> build=<ts> transport=spp device="GemPC Twin ESP32"
#   spp: server started (scn 1) — discoverable
```

### Host setup (bluez + pcscd)

```bash
sudo apt install pcscd libccid pcsc-tools bluez

# 1. Bring the host adapter up
sudo rfkill unblock bluetooth && sudo hciconfig hci0 up

# 2. Pair (SSP numeric compare — accept on the host; the firmware
#    auto-accepts its side; legacy fallback PIN is 1234)
bluetoothctl
  [bluetooth]# scan on
  ... "GemPC Twin ESP32" appears ...
  [bluetooth]# pair <BD_ADDR>        # accept the numeric compare
  [bluetooth]# trust <BD_ADDR>       # auto-reconnect without re-prompting
  [bluetooth]# exit

# 3. Expose the SPP service as /dev/rfcomm0
#    Channel 1 is the ESP32 SPP default; confirm via `sdptool browse <BD_ADDR>`
#    (look for the "Serial Port" service SCN).
sudo rfcomm watch hci0 1
#   (on connect: "Connection from <BD_ADDR> to /dev/rfcomm0")

# 4. Point pcscd at it
sudo cp firmware/esp32-ccid/reader.conf.rfcomm /etc/reader.conf.d/GemPCTwin-rfcomm.conf
sudo systemctl restart pcscd

# 5. Verify
pcsc_scan        # "GemPCTwin rfcomm" + ATR once a card is on the coil
```

Cleanup: `sudo rfcomm release hci0 1` (and stop the `rfcomm watch` process).

### Notes + limitations

- **Latency**: expect RFCOMM scheduling latency on top of the serial path
  (Escape 0xD2 decomposes it — firmware turnaround vs link turnaround —
  once a card is back on the coil). The 0xD2/0xD0/0xD1 escapes work over
  SPP exactly as over UART; 0xD1 coredumps settle 100 ms before the panic
  so the ack drains (Cb-mode writes are queued, not flushed).
- **Single client** — one RFCOMM connection at a time (a second client is
  refused by the profile); pcscd is the only intended consumer.
- **Congestion** is handled: responses park and retry on
  `ESP_SPP_CONG_EVT`, order-preserving (see `spp_server::SppLink`).
- **m5stick-ble is unaffected** — `ble` and `spp` remain mutually
  exclusive builds.

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

**Nucula (USB-CDC) gotcha**: `DEVICENAME` uses a `device:reader` split on the
FIRST colon — `/dev/serial/by-id/...` paths contain colons (MAC bytes) and
parse wrong (`Nucula CCID init failed` in the journal). Use the resolved
`/dev/ttyACM0`-style path instead:

```
DEVICENAME  /dev/ttyACM0:GemPCTwin
```

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
| `ccid_handler.rs` | Thin wrapper: delegates CCID dispatch to the shared `ccid-core` handler, keeps the T=1 endpoint routing (t1.rs) local (PERF_AND_DRY_STRATEGY §3.1) |
| `ccid_types.rs` | CCID message structs, RDR_to_PC slot status, data rates |
| `pn532_driver.rs` | PN532 SPI driver: SAM configuration, InListPassiveTarget, InDataExchange (PN532 backend) |
| `mfrc522_driver.rs` | MFRC522 NFC driver: ISO 14443-4 APDU via iso14443 crate (MFRC522 backend) |
| `pn7160_driver.rs` | PN7160 NCI driver over `pn7160-nci` (nucula backend) |
| `pn7160_i2c.rs` | PN7160 I2C + VEN/IRQ transport, verdict-selectable bring-up constructors (target-only) |
| `pn7160_bringup.rs` | PN7160 bring-up mains: NCI init ladder + card heartbeat (target-only) |
| `pn7160_ccid.rs` | nucula CCID main: serves CCID over the USB-Serial/JTAG CDC port (target-only) |
| `ccid_serial_server.rs` | Host-testable GemPC serial CCID serving core: echo, framed response, interval-gated card polling (shared by the USB-CDC main) |
| `log_shim.rs` | REMOVED 2026-10-12 (issue #131): the #91 USB-CDC log ring/drain is retired — the clean-wire ceiling now lands before the driver claim (one boot banner, then framed CCID only; see `docs/LOGGING_DOCTRINE.md` at the repo root) |
| `ble_console.rs` | BLE debug console facade (issue #66): logger install + Bluedroid GATT bring-up + C-log silencing, one `init`/`drain` pair per main (target-only, `ble` feature) |
| `ble_debug.rs` | NUS-lookalike BLE GATT server for the debug console (target-only, `ble` feature) |
| `ble_logger.rs` | `log` crate sink queueing records for the BLE console (target-only, `ble` feature) |
| `ble_log_queue.rs` | Pure log ring buffer + formatting shared by the BLE logger shells (host-tested) |
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
- **Per-block MFRC522 latency (~40 ms/APDU class)** — CAP-scale uploads
  (GlobalPlatform installs) through the m5stick reader stall; pcscd-level
  APDU relay and T=1 are unaffected
- **Nucula (C3) pcscd impossible** — the C3 USB-Serial/JTAG chip-resets on
  the DTR/RTS patterns libccidtwin uses; no disable bit on this silicon
  (issue #117). Direct-serial GemPC probing with the safe opener
  (`dtr=False; rts=False` BEFORE `open()`, settle ≥4 s) is the supported
  test surface; expect ~300 ms/command CDC latency floor (issue #119)
- **Target-specific LED driver** — the M5Stack Atom LED matrix is driven on Xtensa hardware builds; host tests use a stubbed implementation so shared state and pattern logic stay testable without ESP32 hardware
