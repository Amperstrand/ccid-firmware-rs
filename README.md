# CCID Firmware Repository

This repository now carries two firmware products on the same `main` branch:

- `./firmware/ccid-firmware/` — STM32 USB CCID firmware for contact smart cards
- `./firmware/esp32-ccid/` — ESP32 serial CCID firmware for NFC cards

Current supported hardware combinations:

| MCU | Card frontend | Status | Notes |
|-----|---------------|--------|-------|
| STM32F469-DISCO | Specter DIY Shield Lite / ISO 7816 contact slot | Primary | Wired CCID over USB |
| ESP32 (M5Stack Atom Matrix) | MFRC522 over I2C | Primary | NFC CCID over GemPC Twin serial protocol |
| ESP32 dev boards | PN532 over SPI | Secondary | Kept supported, but current focus is MFRC522 |

Near-term direction:

- keep one repository and one `main` branch for both products
- keep target-specific hardware initialization separate
- shared CCID core crates now extracted under `crates/` (protocol, response builders, serial framing)
- remaining work: evaluate usbd-ccid as USB transport, board pin config trait, NFC crate extraction

The practical architecture is a shared CCID core with interchangeable axes:

- MCU target: STM32 or ESP32
- card frontend: wired contact or NFC
- transport: USB CCID or serial CCID

## Repository layout

- `crates/ccid-protocol/` — shared CCID protocol types, constants, ATR parsing
- `crates/card-interface/` — shared card frontend trait and types
- `crates/ccid-core/` — shared CCID response builders, PPS validation, parameter lookup
- `crates/ccid-transport-serial/` — GemPC Twin serial CCID framing
- `firmware/ccid-firmware/` — STM32 contact-reader firmware
- `firmware/esp32-ccid/` — ESP32 NFC-reader firmware
- `host-tools/` — host-side PC/SC utilities
- `vendor/synopsys-usb-otg/` — STM32 USB dependency
- `vendor/mfrc522/` — patched MFRC522 dependency used by ESP32
- `vendor/iso14443-rs/` — tracked ISO 14443 protocol crate used by ESP32 MFRC522

## Quick start

### STM32 contact CCID

- Build: `cargo build --release --target thumbv7em-none-eabihf`
- Flash: `probe-rs run --chip STM32F469NI target/thumbv7em-none-eabihf/release/ccid-firmware`
- Details: [`BUILDING.md`](BUILDING.md)

### ESP32 NFC CCID

- Build: `cargo +esp build --release` in `firmware/esp32-ccid/`
- Flash/test helper: `firmware/esp32-ccid/flash_and_test.sh`
- Details: [`firmware/esp32-ccid/README.md`](firmware/esp32-ccid/README.md)

## STM32 contact CCID firmware

Rust firmware for STM32-based USB CCID readers.

- USB identity: IDBridge CT30-compatible (`VID:PID 08E6:3437`)
- Smartcard protocols: T=0 and T=1
- Transport: ISO 7816-3 over USART2 smartcard mode
- Host compatibility: pcscd/libccid and standard CCID stacks

## CCID Specification Compliance

This implementation targets **98%+ compliance** with CCID Rev 1.1 specification.

**Spec Reference**: [DWG_Smart-Card_CCID_Rev110.pdf](https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf)

### Implemented Commands

| Command | Spec § | Status | Notes |
|---------|--------|--------|-------|
| IccPowerOn | 6.1.1 | ✅ Full | dwLength validated |
| IccPowerOff | 6.1.2 | ✅ Full | |
| GetSlotStatus | 6.1.3 | ✅ Full | |
| XfrBlock | 6.1.4 | ✅ Full | Short APDU level |
| GetParameters | 6.1.5 | ✅ Full | T=0 and T=1 |
| SetParameters | 6.1.7 | ✅ Full | With libccid quirk handling |
| ResetParameters | 6.1.6 | ✅ Full | |
| IccClock | 6.1.9 | ✅ Full | Clock start/stop |
| Secure (PIN) | 6.1.11/12 | ✅ Full | **Exceeds osmo-ccid-firmware** |
| SetDataRateAndClockFrequency | 6.1.14 | ✅ Full | |
| Abort | 6.1.13 | ⚠️ Stub | Accepts, no async abort |
| Escape | 6.1.8 | ⚠️ Stub | Returns CMD_NOT_SUPPORTED |
| T0APDU | 6.1.10 | ⚠️ Stub | Returns CMD_NOT_SUPPORTED |
| Mechanical | 6.1.12 | ⚠️ Stub | Returns CMD_NOT_SUPPORTED |

### Stub Rationale

The stubbed commands are intentional:
- **Escape**: Vendor-specific, no standard behavior defined
- **T0APDU**: TPDU-level control, we use Short APDU level
- **Mechanical**: No mechanical card eject/capture hardware
- **Abort**: Single-slot synchronous reader, no async operations to abort

### Detailed Audit

See `docs/CCID_SPEC_AUDIT.md` for the full specification compliance audit with code references.

### Comparison with osmo-ccid-firmware

| Feature | osmo-ccid-firmware | This Implementation |
|---------|---------------------|---------------------|
| PIN Verify (Secure) | CMD_NOT_SUPPORTED stub | ✅ Full implementation |
| PIN Modify (Secure) | CMD_NOT_SUPPORTED stub | ✅ Full implementation |
| Parameter persistence | proposed_pars pattern | Direct ATR-derived |
| Multi-slot support | Yes (8 slots) | Single slot |
| Async TPDU | Yes | Synchronous APDU |

**See also**: `docs/AUDIT_PLAN.md` for a structured comparison suitable for formal auditing.

### Future: Embassy Migration

This implementation uses **blocking I/O** which is appropriate for single-slot synchronous APDU-level readers. If migrating to Embassy's async runtime:

| Aspect | Current (Blocking) | Embassy (Async) | Trigger for Change |
|--------|---------------------|-----------------|---------------------|
| Command handling | `fn handle_*(&mut self, ...)` | `async fn handle_*(&mut self, ...)` | Multi-slot support |
| Card I/O | `self.driver.transmit_apdu()` blocks | `self.driver.transmit_apdu().await` | Overlapping operations |
| Parameter persistence | Direct ATR params | `proposed_pars` pattern | TPDU-level mode |
| Time Extension | Not needed | Required for long ops | Async card operations |
| Abort | Stub (no async ops) | Full abort propagation | Multi-slot readers |

**Code change example**:
```rust
// Current: Direct commit
fn handle_set_parameters(&mut self, seq: u8, params: AtrParams) {
    self.current_protocol = params.protocol;
}

// Embassy: Defer commit until card operation succeeds
async fn handle_set_parameters(&mut self, seq: u8, params: AtrParams) {
    self.proposed_pars = Some(params);
    // Later: commit after successful card operation
    self.current_pars = self.proposed_pars.take();
}
```

See `docs/AUDIT_PLAN.md` for complete migration checklist.

## Licensing and provenance

This project contains derivative ideas and protocol behavior from `osmo-ccid-firmware` and is licensed as **GPL-2.0-or-later**. See `LICENSE` for details.

## Hardware

- Current target: STM32F469-DISCO (+ Specter DIY Shield Lite)
- Goal: extend to additional STM32 targets
- Smartcard slot: ISO 7816 contact card interface
- USB: OTG FS (PA11/PA12)
- Detailed pin map: `PINOUT.md`

## Build

Quick start (default Cherry SmartTerminal ST-2xxx profile):

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

GitHub Actions workflow at `.github/workflows/ci.yml` runs host-safe validation for both products:

- STM32 build/lint/test jobs at repository root
- ESP32 host-side tests in `firmware/esp32-ccid/`
- vendored `iso14443-rs` host-side tests
- Python syntax checks for helper scripts

## Device Profiles

Reference: `reference/CCID/readers/*.txt` (authoritative device specifications)

| Profile Feature | Device | VID:PID | PIN Pad | Default |
|-----------------|--------|---------|---------|---------|
| `profile-cherry-smartterminal-st2xxx` | Cherry SmartTerminal ST-2xxx | `046a:003e` | ✓ Yes | ✓ |
| `profile-gemalto-idbridge-ct30` | Gemalto IDBridge CT30 | `08e6:3437` | No | |
| `profile-gemalto-idbridge-k30` | Gemalto IDBridge K30 | `08e6:3438` | No | |

> **⚠️ IMPORTANT:** Only the **Cherry ST-2xxx** has PIN pad support.
> The K30 (PID:3438) is a basic reader, virtually identical to CT30 (PID:3437).

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for release notes and version history.

## Repository guide

- Firmware entrypoint: `firmware/ccid-firmware/src/main.rs`
- Smartcard transport: `firmware/ccid-firmware/src/smartcard.rs`
- CCID protocol handling: `firmware/ccid-firmware/src/ccid.rs`
- Unit-testable protocol helpers: `firmware/ccid-firmware/src/protocol_unit.rs`
- Unit tests inspired by osmo test files: `firmware/ccid-firmware/src/protocol_unit.rs` (`#[cfg(test)]` module)
- Pinout: `PINOUT.md`
