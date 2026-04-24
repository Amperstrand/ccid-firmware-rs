# Building CCID Firmware

This repository now contains two firmware packages on the same `main` branch:

- `./` — STM32 USB CCID firmware for contact smart cards
- `./esp32-ccid/` — ESP32 serial CCID firmware for NFC cards

Current practical focus:

- STM32 + Specter DIY Shield Lite / contact smart-card slot
- ESP32 + MFRC522 over I2C

PN532 remains supported in the ESP32 package, but MFRC522 is the primary NFC path right now.

## Build the right package

The repository root has a default Cargo target of `thumbv7em-none-eabihf` in `.cargo/config.toml`. That is correct for STM32, but it means ESP32 commands should be run from `esp32-ccid/` (or with an explicit target override) so they do not inherit the STM32 default.

## Repository layout

- `src/` — STM32 firmware
- `esp32-ccid/` — ESP32 firmware
- `vendor/mfrc522/` — tracked MFRC522 dependency used by ESP32
- `vendor/iso14443-rs/` — tracked ISO 14443 dependency used by the ESP32 MFRC522 path

## ESP32 NFC firmware

### Prerequisites

Install the ESP-IDF toolchain and Rust Xtensa support as documented in the ESP-IDF and `espup` setup guides.

Typical one-time Rust target setup:

```bash
rustup target add xtensa-esp32-espidf
```

### Build

Run these commands from `esp32-ccid/`.

**Default build (MFRC522 backend):**

```bash
cargo +esp build --release
```

**Explicit MFRC522 build:**

```bash
cargo +esp build --release --features backend-mfrc522
```

**PN532 build:**

```bash
cargo +esp build --release --no-default-features --features backend-pn532
```

Binary location:

```text
esp32-ccid/target/xtensa-esp32-espidf/release/esp32-ccid
```

### Flash and host verification

- Flash and basic verification helper: `esp32-ccid/flash_and_test.sh`
- Host setup helper: `esp32-ccid/setup.sh`
- Reader documentation: `esp32-ccid/README.md`

### Host-side tests (safe without hardware)

Run from `esp32-ccid/`:

```bash
cargo test --target x86_64-unknown-linux-gnu
```

The vendored ISO 14443 crate used by the MFRC522 path also has host-safe tests:

```bash
cd ../vendor/iso14443-rs
cargo test --features std --target x86_64-unknown-linux-gnu
```

## STM32 contact firmware

This section documents how to build the STM32F469-DISCO CCID firmware for different device profiles.

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

GitHub Actions workflow (`.github/workflows/ci.yml`) runs host-safe validation for both firmware packages:

- STM32 profile builds at the repository root
- STM32 host-side tests and linting
- ESP32 host-side tests in `esp32-ccid/`
- Vendored `iso14443-rs` host-side tests used by the ESP32 MFRC522 path

The current workflow intentionally avoids requiring a full ESP-IDF/Xtensa toolchain in every CI job. Xtensa release builds are still expected locally before shipping ESP32 firmware.

## References

- Hardware pinout: `PINOUT.md`
- Source organization: `README.md`
- Publication readiness: `PUBLICATION_READINESS.md`
- stm32f4xx-hal: https://github.com/Amperstrand/stm32f4xx-hal
- probe-rs: https://probe.rs
