# STM32 CCID Firmware

Rust firmware for STM32-based USB CCID smartcard readers.

## Features

- **USB Identity**: IDBridge CT30-compatible (`VID:PID 08E6:3437`) for plug-and-play Linux
- **Protocols**: T=0 and T=1 (ISO 7816-3)
- **Transport**: USART smartcard mode with hardware CLK generation
- **Host Compatibility**: pcscd/libccid, OpenSC, standard CCID stacks

## Hardware

**Current target**: STM32F469-DISCO + Specter DIY Shield Lite

| Component | MCU Pin | Notes |
|-----------|---------|-------|
| Smartcard I/O | PA2 | USART2_TX (AF7, open-drain) |
| Smartcard CLK | PA4 | USART2_CK (AF7, push-pull) |
| Card RST | PG10 | GPIO output |
| Card PWR | PC5 | Active-low power gate |
| Card Detect | PC2 | HIGH = card present |
| USB DM | PA11 | OTG FS |
| USB DP | PA12 | OTG FS |

See `PINOUT.md` for complete pinout.

## Build

```bash
# Install target (first time only)
rustup target add thumbv7em-none-eabihf

# Build release firmware
cargo build --release
```

**Note:** Release mode is **required** for reliable USB. The `synopsys-usb-otg` driver has timing issues in debug builds.

## Flash

Two methods are supported:

### Method 1: probe-rs (recommended)

```bash
# Flash and run
probe-rs run --chip STM32F469NIHx --release

# Or flash without running
probe-rs download --chip STM32F469NIHx --release --verify
```

### Method 2: st-flash (ST-Link)

```bash
# Generate binary
cargo objcopy --release -- -O binary ccid-firmware.bin

# Flash with st-flash
st-flash write ccid-firmware.bin 0x08000000
```

### Method 3: STM32CubeProgrammer

Use the ELF file directly:
```
target/thumbv7em-none-eabihf/release/ccid-firmware
```

## Firmware Output Files

| File | Format | Size | Usage |
|------|--------|------|-------|
| `ccid-firmware` | ELF | ~1.7MB | Debugging with GDB, symbols included |
| `ccid-firmware.bin` | Raw binary | ~44KB | Direct flash with st-flash, bootloaders |
| `ccid-firmware.hex` | Intel HEX | ~120KB | Some flash tools prefer this |

Generate additional formats:
```bash
# Raw binary
cargo objcopy --release -- -O binary ccid-firmware.bin

# Intel HEX
cargo objcopy --release -- -O ihex ccid-firmware.hex
```

## Testing

### CI Tests (automated)

```bash
# Run host-side unit tests
cargo test --target x86_64-unknown-linux-gnu

# Python syntax check
python3 -m py_compile test_*.py
```

### Hardware Tests (manual)

Prerequisites:
- STM32F469-DISCO board with smartcard slot
- Test card (T=0 or T=1)
- pcscd running on host

```bash
# 1. Flash firmware
probe-rs run --chip STM32F469NIHx --release

# 2. Verify USB enumeration
lsusb | grep -i gemalto
# Expected: Bus XXX Device XXX: ID 08e6:3437 Gemalto

# 3. Check pcscd sees the reader
pcsc_scan

# 4. Run generic smartcard test
python3 test_smartcard.py

# 5. Run CCID APDU test
python3 test_ccid_apdu.py
```

See `VERIFICATION.md` for detailed verification procedures.

## License

GPL-2.0-or-later. See `LICENSE` for details.

This project contains derivative ideas from `osmo-ccid-firmware`.

## Repository Structure

```
src/
├── main.rs          # Hardware init, USB setup, main loop
├── ccid.rs          # CCID protocol handler
├── smartcard.rs     # ISO 7816-3 driver, T=0/T=1
├── t1_engine.rs     # T=1 block protocol
└── usb_identity.rs  # USB VID/PID constants

test_*.py            # Host-side test scripts
PINOUT.md            # Hardware pin reference
VERIFICATION.md      # Testing procedures
```

## Reproducible Builds

This project supports reproducible builds for firmware verification.

### Prerequisites

- `rust-toolchain.toml` pins Rust version (1.85)
- `Cargo.lock` pins dependencies
- `SOURCE_DATE_EPOCH` set for deterministic timestamps

### Local Build

```bash
cargo build --release
```

### Docker Build (recommended for releases)

```bash
# Build in isolated environment
docker build -t ccid-firmware-builder .

# Extract firmware
docker run --rm -v $(pwd)/output:/app/target ccid-firmware-builder
cp output/thumbv7em-none-eabihf/release/ccid-firmware ./ccid-firmware.bin
```

### Verify Reproducibility

```bash
# Build twice and compare
cargo build --release
cp target/thumbv7em-none-eabihf/release/ccid-firmware build1.bin
cargo clean
cargo build --release
cp target/thumbv7em-none-eabihf/release/ccid-firmware build2.bin
sha256sum build1.bin build2.bin
# Should show identical hashes
rm build1.bin build2.bin
```

---

## Release Checklist

Before publishing a release:

- [ ] `cargo build --release` succeeds
- [ ] `cargo test --target x86_64-unknown-linux-gnu` passes
- [ ] Firmware flashed and enumerated via USB
- [ ] `pcsc_scan` shows reader with correct identity
- [ ] `test_smartcard.py` passes with test card
- [ ] Generate `.bin` and `.hex` files
- [ ] Create SHA256 checksums
- [ ] Tag release in git
