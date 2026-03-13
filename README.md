# STM32 CCID Firmware

Rust firmware for STM32-based USB CCID readers.

- USB identity: IDBridge CT30-compatible (`VID:PID 08E6:3437`)
- Smartcard protocols: T=0 and T=1
- Transport: ISO 7816-3 over USART2 smartcard mode
- Host compatibility: pcscd/libccid and standard CCID stacks

## Licensing and provenance

This project contains derivative ideas and protocol behavior from `osmo-ccid-firmware` and is licensed as **GPL-2.0-or-later**. See `LICENSE` for details.

## Hardware

- Current target: STM32F469-DISCO (+ Specter DIY Shield Lite)
- Goal: extend to additional STM32 targets
- Smartcard slot: ISO 7816 contact card interface
- USB: OTG FS (PA11/PA12)
- Detailed pin map: `PINOUT.md`

## Build

Quick start (default Cherry ST-2100 profile):

```bash
cargo build --release --target thumbv7em-none-eabihf
```

**Note:** Release mode is required for reliable USB behavior with `synopsys-usb-otg`.

Detailed build instructions, profile selection, and flashing are documented in [BUILDING.md](BUILDING.md).

## Linux note (plug and play)

With the default `08E6:3437` identity, Linux `pcscd/libccid` works out of the box on standard
installations that already include the upstream reader list.

## Flash

```bash
probe-rs run --chip STM32F469NI target/thumbv7em-none-eabihf/release/ccid-firmware
```

## Test strategy

There are two test categories:

1. Non-hardware unit tests (safe for CI)
2. Manual hardware integration tests (explicitly non-destructive)

### Run unit tests locally

```bash
cargo test --target x86_64-unknown-linux-gnu
```

### Run hardware tests locally (manual only)

See `tests/hardware/README.md`.

- SeedKeeper flow: read-only APDUs, no write/update operations
- sysmocom SIM flow: read/export only, no profile modification commands

These hardware tests are intentionally not executed in CI.

## CI

GitHub Actions workflow at `.github/workflows/ci.yml` runs only host-safe tests:

- Rust unit tests (`cargo test` on host target)
- Python syntax checks for helper scripts

## Device Profiles

| Profile | Device | VID:PID | Default |
|---------|--------|---------|---------|
| `profile-cherry-st2100` | Cherry SmartTerminal ST-2100 (PIN pad) | `046a:003e` | ✓ |
| `profile-gemalto-plain` | Gemalto IDBridge CT30 (basic) | `08e6:3437` | |
| `profile-gemalto-pinpad` | Gemalto IDBridge K30 (PIN pad) | `08e6:3437` | |

Device-specific configurations and build instructions are documented in [BUILDING.md](BUILDING.md).

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for release notes and version history.

## Repository guide

- Firmware entrypoint: `src/main.rs`
- Smartcard transport: `src/smartcard.rs`
- CCID protocol handling: `src/ccid.rs`
- Unit-testable protocol helpers: `src/protocol_unit.rs`
- Unit tests inspired by osmo test files: `src/protocol_unit.rs` (`#[cfg(test)]` module)
- Pinout: `PINOUT.md`
