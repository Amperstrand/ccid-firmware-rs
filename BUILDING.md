# Building CCID Firmware

This guide documents how to build the CCID firmware for different hardware targets.

## Build Variants

| Variant | Hardware | Interface | Feature Flag |
|---------|----------|-----------|-------------|
| **STM32 Contact Card** | STM32F469-DISCO | ISO 7816 UART (contact) | Default (profile features) |
| **NFC Contactless** | ESP32-S3 + PN532 | ISO 14443-4 / ISO-DEP (NFC) | `--features nfc` |

---

## STM32 Contact Card Build (Default)

This is the original build target — a USB CCID smartcard reader using
the STM32F469-DISCO board with ISO 7816 contact-card interface.

## Prerequisites

### Rust Toolchain

Install Rust with the embedded ARM target:

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Add ARM Cortex-M target
rustup target add thumbv7em-none-eabihf
```

### ARM Tools

For binary conversion and flashing:

```bash
# macOS (Homebrew)
brew install arm-none-eabi-binutils

# Linux (Debian/Ubuntu)
sudo apt-get install binutils-arm-none-eabi

# Linux (Fedora)
sudo dnf install arm-none-eabi-binutils-generic
```

### Flashing Tools

Choose one:

**Option A: probe-rs (recommended)**
```bash
cargo install probe-rs --features cli
```

**Option B: st-flash (STLink)**
```bash
# macOS
brew install stlink

# Linux (Debian/Ubuntu)
sudo apt-get install stlink-tools

# Linux (Fedora)
sudo dnf install stlink-tools
```

## Device Profiles

Reference: `reference/CCID/readers/*.txt` (authoritative device specifications)

The firmware supports three mutually exclusive USB device profiles:

| Profile Feature | Device | VID:PID | PIN Pad | Default |
|-----------------|--------|---------|---------|---------|
| `profile-cherry-smartterminal-st2xxx` | Cherry SmartTerminal ST-2xxx | 046a:003e | ✓ Yes | ✓ |
| `profile-gemalto-idbridge-ct30` | Gemalto IDBridge CT30 | 08e6:3437 | No | |
| `profile-gemalto-idbridge-k30` | Gemalto IDBridge K30 | 08e6:3438 | No | |

> **⚠️ IMPORTANT:** Only the **Cherry ST-2xxx** has PIN pad support.
> The K30 (PID:3438) is a basic reader, virtually identical to CT30 (PID:3437).

## Building

### Default Build (Cherry SmartTerminal ST-2xxx)

```bash
cargo build --release
```

Binary location: `target/thumbv7em-none-eabihf/release/ccid-firmware`

**Note:** Release mode is required for reliable USB behavior with `synopsys-usb-otg`.

### Profile-Specific Builds

Build for a specific device profile using the `--no-default-features` and `--features` flags:

**Gemalto CT30 (basic reader):**
```bash
cargo build --release --no-default-features --features profile-gemalto-idbridge-ct30
```

**Gemalto K30 (basic reader):**
```bash
cargo build --release --no-default-features --features profile-gemalto-idbridge-k30
```

**Cherry ST-2xxx (explicit, same as default):**
```bash
cargo build --release --no-default-features --features profile-cherry-smartterminal-st2xxx
```

Binary location for all profiles: `target/thumbv7em-none-eabihf/release/ccid-firmware`

### Development Build

For faster iteration (not recommended for deployment):

```bash
cargo build --release
```

The `[profile.dev]` section uses `opt-level = 1` for faster builds, but release mode is still preferred for USB stability.

## Binary Conversion

The ELF binary must be converted to binary format for flashing:

```bash
arm-none-eabi-objcopy -O binary \
  target/thumbv7em-none-eabihf/release/ccid-firmware \
  ccid-firmware.bin
```

Generate a checksum for verification:

```bash
sha256sum ccid-firmware.bin > ccid-firmware.bin.sha256
```

## Flashing

### With probe-rs (recommended)

Run directly from the build output:

```bash
probe-rs run --chip STM32F469NI target/thumbv7em-none-eabihf/release/ccid-firmware
```

Flash pre-built binary:

```bash
probe-rs download --chip STM32F469NI ccid-firmware.bin --format binary --base-address 0x08000000
```

### With st-flash (STLink)

First, convert the ELF to binary:

```bash
arm-none-eabi-objcopy -O binary \
  target/thumbv7em-none-eabihf/release/ccid-firmware \
  ccid-firmware.bin
```

Then flash:

```bash
st-flash write ccid-firmware.bin 0x8000000
```

## Docker Build (Reproducible)

For reproducible builds in a containerized environment:

```bash
docker build --build-arg PROFILE=profile-gemalto-idbridge-ct30 .
```

The Dockerfile produces the binary at `/app/output/ccid-firmware.bin` with a checksum.

### Verify Reproducibility

Test that builds are deterministic:

```bash
scripts/verify-reproducibility.sh profile-cherry-smartterminal-st2xxx
```

Exit codes:
- `0`: Hashes match (reproducible)
- `1`: Hashes differ (NOT reproducible)
- `2`: Build failed

The script builds the firmware twice in Docker and compares SHA256 checksums. Artifacts are preserved in `.reproducibility-test/` if builds differ.

## Build Artifacts

After `cargo build --release`:

```
target/thumbv7em-none-eabihf/release/
├── ccid-firmware          # ELF binary (with debug info)
├── ccid-firmware.d        # Dependency file
└── deps/                  # Dependencies
```

Key directories:
- **ELF binary:** `target/thumbv7em-none-eabihf/release/ccid-firmware`
- **Binary (after objcopy):** `ccid-firmware.bin`
- **Debug symbols:** Embedded in ELF (use `arm-none-eabi-gdb` for debugging)

## Build Configuration

### Profile Settings (Cargo.toml)

```toml
[profile.release]
debug = 2           # Full DWARF debug info (probe-rs RTT location info)
opt-level = "z"     # Optimize for size (embedded target)
lto = true          # Link-time optimization
codegen-units = 1   # Better optimization (slower compile)
panic = "abort"     # No unwinding (reduces binary size)
```

These settings ensure:
1. Reliable USB behavior with `synopsys-usb-otg`
2. Small firmware size for embedded flash constraints
3. Deterministic output for reproducibility

### Target Specification

Target: **thumbv7em-none-eabihf** (ARMv7-EM with hardware float, bare-metal)

Add to `rust-toolchain.toml` if needed:

```toml
[toolchain]
channel = "stable"
targets = ["thumbv7em-none-eabihf"]
```

## Cleaning

Remove build artifacts:

```bash
cargo clean
```

Remove reproducibility test artifacts:

```bash
rm -rf .reproducibility-test/
```

## Troubleshooting

### Build fails with "target not installed"

```bash
rustup target add thumbv7em-none-eabihf
```

### probe-rs cannot find device

Ensure the STM32F469-DISCO is connected and the STLink firmware is up-to-date:

```bash
probe-rs info
```

### Binary size exceeds flash

Check optimization settings in `Cargo.toml` and ensure `opt-level = "z"` is set.

Use `arm-none-eabi-nm --size-sort target/thumbv7em-none-eabihf/release/ccid-firmware` to identify large symbols.

### USB not recognized after flashing

1. Verify correct profile is built for your device (check VID:PID in firmware code)
2. Ensure full build with `--release` (development builds may have USB issues)
3. Try power-cycling the device or erasing flash before reprogramming

## Testing

### Unit Tests (Host)

Run without hardware:

```bash
cargo test --target x86_64-unknown-linux-gnu
```

### Hardware Tests (Manual)

See `tests/hardware/README.md` for integration tests with actual smartcards.

## Continuous Integration

GitHub Actions workflow (`.github/workflows/ci.yml`) builds and tests automatically:

- Compiles for `thumbv7em-none-eabihf` target
- Runs unit tests on host target (`x86_64-unknown-linux-gnu`)
- Validates all three device profiles

---

## NFC Contactless Build (ESP32-S3 + PN532)

This variant builds a USB CCID NFC reader using an ESP32-S3 microcontroller
and a PN532 NFC frontend module. The host PC sees a standard USB CCID reader;
the NFC side communicates with ISO 14443-4 (ISO-DEP) contactless cards.

### Why ESP32-S3?

| ESP32 Variant | Native USB OTG | Can do CCID? |
|---------------|---------------|-------------|
| ESP32 (original) | ❌ No (UART bridge only) | ❌ No |
| ESP32-C3 | ❌ No (USB Serial/JTAG only) | ❌ No |
| ESP32-C6 | ❌ No (USB Serial/JTAG only) | ❌ No |
| ESP32-S2 | ✅ Yes | ✅ Yes (single core) |
| **ESP32-S3** | ✅ Yes | ✅ **Yes (dual core, recommended)** |

USB CCID requires a **USB device class** endpoint, which needs native USB OTG
hardware. The ESP32-S3 is the best choice: dual-core Xtensa, 512 KB SRAM,
native USB OTG, and excellent Rust support via the `esp-rs` ecosystem.

### Architecture

```
Host PC ←→ [USB CCID] ←→ ESP32-S3 ←→ [SPI] ←→ PN532 ←→ [13.56 MHz RF] ←→ NFC Card
```

**Reused from STM32 path:**
- `src/ccid_core.rs` — CCID protocol message handler (unchanged)
- `src/driver.rs` — `SmartcardDriver` trait (unchanged)
- `src/protocol_unit.rs` — ATR parsing utilities (unchanged)
- `src/pinpad/` — PIN buffer, state machine, APDU builders (unchanged)

**New for NFC path:**
- `src/nfc/mod.rs` — NFC module, device profile, synthetic ATR builder
- `src/nfc/pn532.rs` — PN532 SPI protocol driver
- `src/nfc/pn532_driver.rs` — `SmartcardDriver` impl via PN532
- `src/main_nfc.rs` — ESP32-S3 entry point (hardware init template)

### Prerequisites (NFC)

#### ESP32-S3 Rust Toolchain

Install the ESP32 Rust toolchain using `espup`:

```bash
# Install espup
cargo install espup

# Install Xtensa Rust toolchain + LLVM fork
espup install

# Source the environment (add to .bashrc/.zshrc for persistence)
. ~/export-esp.sh
```

#### Flash Tool

```bash
cargo install espflash
```

#### Hardware Setup

1. **PN532 module** — Set DIP switches to **SPI mode** (typically SEL0=LOW, SEL1=HIGH)
2. **Wiring** (Bolty-style):

   | PN532 Pin | ESP32-S3 Pin | Function |
   |-----------|-------------|----------|
   | SCK | GPIO 12 | SPI Clock |
   | MOSI | GPIO 11 | SPI Data Out |
   | MISO | GPIO 13 | SPI Data In |
   | SS/CS | GPIO 10 | Chip Select |
   | VCC | 3.3V | Power |
   | GND | GND | Ground |

3. **USB** — Connect via the **native USB** port (not the UART port)

### Building (NFC)

```bash
# Build the NFC variant
cargo build --release --features nfc --no-default-features \
    --bin ccid-nfc --target xtensa-esp32s3-none-elf
```

### Flashing (NFC)

```bash
espflash flash target/xtensa-esp32s3-none-elf/release/ccid-nfc
```

### NFC Testing (Host)

The NFC module's library code (PN532 driver, synthetic ATR, profile) is
tested on the host without hardware:

```bash
cargo test --target x86_64-unknown-linux-gnu --features nfc -- nfc
```

### Supported Card Types (v1)

- **ISO 14443-4 Type A** (ISO-DEP / APDU-capable):
  - JavaCard applets (e.g., SeedKeeper, Satochip)
  - YubiKey NFC
  - JCOP cards
  - Any card with SAK bit 5 set (ISO-DEP indicator)

Not yet supported:
- ISO 14443-4 Type B
- MIFARE Classic / Ultralight (non-APDU)
- NFC Forum tag types (NDEF)

### Current Limitations

- `src/main_nfc.rs` is a **structural template** — it shows the intended
  initialization flow but requires the ESP32-S3 HAL dependencies to compile
  for the Xtensa target. See comments in the file for details.
- USB OTG setup needs implementation using the `esp-hal` USB peripheral.
- The `CcidClass` USB transport wrapper (`src/ccid.rs`) currently uses
  `usb-device` with STM32-specific endpoint allocation. A similar wrapper
  is needed for ESP32-S3 USB OTG.
- NFC card presence detection (`is_card_present()`) currently relies on
  cached state. True RF polling can be added later.

### Next Steps

1. Add `esp-hal` dependencies (requires `espup` toolchain)
2. Implement ESP32-S3 SPI and USB OTG initialization in `main_nfc.rs`
3. Create or adapt a `CcidClass` wrapper for the ESP32-S3 USB peripheral
4. Hardware integration testing with real NFC cards
5. Add ISO 14443-4 Type B support in the PN532 driver

## References

- Hardware pinout: `PINOUT.md`
- Source organization: `README.md`
- Publication readiness: `PUBLICATION_READINESS.md`
- stm32f4xx-hal: https://github.com/Amperstrand/stm32f4xx-hal
- probe-rs: https://probe.rs
- esp-rs: https://github.com/esp-rs
- espup: https://github.com/esp-rs/espup
- PN532 User Manual: https://www.nxp.com/docs/en/user-guide/141520.pdf
