# Building CCID Firmware

This guide documents how to build the STM32F469-DISCO CCID firmware for different device profiles.

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

The firmware supports three mutually exclusive USB device profiles:

| Profile | Device | VID:PID | Default |
|---------|--------|---------|---------|
| `profile-cherry-st2100` | Cherry SmartTerminal ST-2100 (PIN pad) | 046a:003e | ✓ |
| `profile-gemalto-plain` | Gemalto IDBridge CT30 (basic) | 08e6:3437 | |
| `profile-gemalto-pinpad` | Gemalto IDBridge K30 (PIN pad) | 08e6:3437 | |

## Building

### Default Build (Cherry ST-2100)

```bash
cargo build --release
```

Binary location: `target/thumbv7em-none-eabihf/release/ccid-firmware`

**Note:** Release mode is required for reliable USB behavior with `synopsys-usb-otg`.

### Profile-Specific Builds

Build for a specific device profile using the `--no-default-features` and `--features` flags:

**Gemalto CT30 (plain reader, no PIN pad):**
```bash
cargo build --release --no-default-features --features profile-gemalto-plain
```

**Gemalto K30 (PIN pad reader):**
```bash
cargo build --release --no-default-features --features profile-gemalto-pinpad
```

**Cherry ST-2100 (explicit, same as default):**
```bash
cargo build --release --no-default-features --features profile-cherry-st2100
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
docker build --build-arg PROFILE=profile-gemalto-plain .
```

The Dockerfile produces the binary at `/app/output/ccid-firmware.bin` with a checksum.

### Verify Reproducibility

Test that builds are deterministic:

```bash
scripts/verify-reproducibility.sh profile-cherry-st2100
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

## References

- Hardware pinout: `PINOUT.md`
- Source organization: `README.md`
- Publication readiness: `PUBLICATION_READINESS.md`
- stm32f4xx-hal: https://github.com/Amperstrand/stm32f4xx-hal
- probe-rs: https://probe.rs
