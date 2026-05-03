# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-05-03

### Added
- **Shared CCID architecture** — four new shared crates under `crates/`:
  - `ccid-protocol` — protocol types, constants, ATR parsing (moved from root)
  - `card-interface` — card frontend trait and types (no_std)
  - `ccid-core` — CCID response builders, PPS validation, parameter lookup (21 tests)
  - `ccid-transport-serial` — GemPC Twin serial CCID framing (25 tests)
- **Workspace directory reorganization** (Phase 6):
  - `firmware/ccid-firmware/` — STM32 USB CCID firmware (moved from root)
  - `firmware/esp32-ccid/` — ESP32 serial CCID firmware (moved from `esp32-ccid/`)
  - `crates/ccid-protocol/` — shared protocol (moved from `ccid-protocol/`)
  - Root `Cargo.toml` is now a pure workspace manifest with profiles and patches
- **ESP32 ccid_handler refactor** — uses shared `ccid_core` response builders, PPS validation, parameter lookup instead of local duplicates
- **PresenceState** — defined once in `card-interface`, shared across STM32 and ESP32
- **CI** — F746 build matrix entries now include profile feature flag
- 337 total tests across all workspace members

### Changed
- F746 card clock increased from 1 MHz to 5 MHz (ISO 7816 maximum), 2x APDU throughput
- All 5 clippy warnings in STM32 firmware eliminated (semantic no-ops)
- `BUILDING.md`, `README.md`, `Dockerfile`, CI workflow updated for new directory layout

### Fixed
- F746 performance: card clock 1->5 MHz (hardware verified at 74.4ms avg round-trip)
- CI F746 build entries missing profile feature flag
- `replay_seedkeeper_full_session` test: expected byte corrected for ATR-derived TB3 params

### Hardware Verification (May 2026)
- STM32F746-DISCO (Cherry ST-2xxx USB CCID): 74.4ms avg, ComSign eID T=1 contact card
- Both F746 and F469 firmware builds verified clean
- ESP32 hardware testing pending (M5Stack Atom disconnected)

## [0.1.0] - 2026-04-24

### Added
- **ESP32 NFC CCID firmware integrated into main branch** (`esp32-ccid/`)
  - MFRC522 NFC backend over I2C (M5Stack Atom Matrix) — primary NFC path
  - PN532 NFC backend over SPI — secondary, remains supported
  - GemPC Twin serial CCID protocol over UART0 (115200 8N2)
  - 75 host-side unit tests covering serial framing, CCID parsing, NFC logic, LED patterns
  - WS2812 LED diagnostic patterns (init, ready, card present, TxRx, error)
  - BLE debug logger (optional, behind `backend-mfrc522` feature)
- **Vendored patched dependencies** under `vendor/`
  - `vendor/iso14443-rs/` — patched ISO 14443 protocol crate (PcdSession, try_set_timeout_ms, set_fsc, set_base_fwt_ms)
  - `vendor/mfrc522/` — patched MFRC522 driver crate
- **CI coverage for both products**: STM32, ESP32, and iso14443 host-test jobs
- **esp-idf-svc 0.52.1** from crates.io with ESP-IDF 5.2.4 pin

### Hardware Verification (2026-04-24)
- **ESP32 + MFRC522 (GemPC Twin serial):** pcscd detects reader, NFC card responds
  - Card: NXP P71 SmartMX3 P71D320 JCOP4 JavaCard
  - ATR: `3B 85 80 01 80 73 C8 21 10 0E` (TCK correct)
  - Reader: `GemPCTwin serial 00 00`
- **STM32 + Specter DIY Shield (Cherry ST-2xxx USB CCID):** pcscd detects reader, contact card responds
  - Card: ComSign eID (T=1, IFSC=254)
  - ATR: `3B D5 18 FF 81 91 FE 1F C3 80 73 C8 21 10 0A` (TCK correct)
  - Reader: `Cherry GmbH SmartTerminal ST-2xxx (ST2XXX-001) 02 00`
- Both readers verified simultaneously on the same host
- All host tests pass: STM32 82/82, ESP32 75/75, iso14443 52/52

### Changed
- `.gitignore`: `vendor/**/target/` instead of blanket `esp32-ccid/vendor/`
- `esp32-ccid/src/led.rs`: Host-build gating with `#[cfg(all(target_arch, feature))]`
- `esp32-ccid/src/ble_debug.rs`, `ble_logger.rs`: Poison-recovering lock helpers
- `README.md`, `BUILDING.md`, `esp32-ccid/README.md`: Two-product documentation

### Notes
- `esp32-serial-ccid` branch merged into `main` via fast-forward + rebase
- Branch `esp32-serial-ccid` deleted after successful merge
- Known FTDI FT232 wedge bug: espflash DTR/RTS toggles require physical USB replug after flash

## [0.0.9] - 2026-03-17

### Added
- **CCID Spec Compliance Documentation**
  - `docs/CCID_SPEC_AUDIT.md`: Comprehensive spec compliance audit
  - `docs/AUDIT_PLAN.md`: Structured comparison of spec vs osmo vs our implementation
  - All CCID command handlers now include spec citations in doc comments

### Changed
- **README.md**: Added CCID compliance section with feature comparison table
- **IccPowerOn**: Now validates dwLength==0 per CCID §6.1.1
- **Spec citations**: All command handlers reference CCID Rev 1.1 spec sections
- **Compliance rating**: Improved from 95% to 98%+

### Fixed
- dwLength validation in IccPowerOn per CCID spec requirement

### Notes
- **Embassy Migration**: Documented requirements for async runtime migration
- **osmo Comparison**: Documented where we exceed osmo (PIN verify/modify) vs match (stubs)
- **Future Work**: Identified what would need to change for multi-slot, async, or TPDU level support

---

## [Unreleased]

### Added
- Added osmo-ccid-firmware as git submodule at `reference/osmo-ccid-firmware/` for protocol reference
- Added `docs/SPECIFICATIONS.md` with links to official CCID, ISO 7816, and PC/SC specifications
- Added compliance review documentation for stub commands (Escape, T0APDU, Mechanical, Abort)

### Changed
- **Profile naming refactored** to align with CCID project conventions:
  - `profile-cherry-st2100` → `profile-cherry-smartterminal-st2xxx`
  - `profile-gemalto-plain` → `profile-gemalto-idbridge-ct30`
  - `profile-gemalto-pinpad` → `profile-gemalto-idbridge-k30`
- **CRITICAL FIX**: Gemalto IDBridge K30 profile no longer falsely claims PIN pad and LCD capabilities
  - Real K30 has bPINSupport=0x00, wLcdLayout=0x0000 (no PIN, no LCD)
  - K30 uses TPDU level (0x00010230), not Short APDU (0x00020472)
  - For PIN pad support, use profile-cherry-smartterminal-st2xxx (the only PIN-capable profile)
- All profiles now match CCID reference files exactly (reference/CCID/readers/*.txt)
- Fixed GEMALTO_K30_DWFEATURES constant: 0x00010230 (was incorrectly 0x00020472)
- Fixed dwFeatures decomposition tests to match actual reference values
- Added upstream CCID project as git submodule for authoritative device reference

### Compliance Notes
- Voltage support verified correct per profile (Cherry: 0x01, Gemalto: 0x07)
- Stub commands (Escape, T0APDU, Mechanical) intentionally return CMD_NOT_SUPPORTED
- Abort command returns success (matches osmo-ccid-firmware behavior for single-slot)
- T=1 prepare_rx() is intentional trait default, not a bug
- Time extension handling requires async architecture (future enhancement)

## [0.0.8] - 2026-03-13

### Changed
- Fixed all clippy errors and warnings (83 total)
- Added crate-level `#![allow(...)]` for scaffolding code (PIN pad features not yet in use)
- Added pre-commit hook for `cargo fmt --check` and `cargo clippy -- -D warnings`
- Improved code quality: replaced manual range checks with `is_ascii_digit()`, used iterator patterns

## [0.0.7] - 2026-03-13

### Changed
- **Streamlined release artifacts**: Only `.bin` files are now released (following specter-diy pattern)
- Dropped `.elf` and `.hex` from releases - `.bin` is sufficient for all flashing tools
- Single `SHA256SUMS` file instead of individual `.sha256` files per artifact
- Release size reduced from 19 files to 4 files (3x `.bin` + 1x `SHA256SUMS`)

## [0.0.6] - 2026-03-13

### Added
- Multi-profile CI/CD: All 3 profiles now built and released
- Reproducibility improvements: Added `--remap-path-prefix` flag to build system for reproducible builds

### Changed
- Release workflow now publishes binaries for all 3 device profiles
- Artifact naming includes profile suffix for clear identification of profile-specific binaries

## [0.0.4] - 2026-03-13

### Fixed
- **Critical Boot Bug - SRAM Overflow**: Fixed memory.x configuration reducing SRAM from 320K to 256K to prevent HardFault on boot
- **Clock Configuration**: Changed HSI to HSE clock source for USB compatibility and stability
- Hardware-verified profiles (Cherry ST-2100 and Gemalto CT30) now working correctly

## [0.0.3] - 2026-03-12

### Changed
- Simplified to ARM-only build, removing x86_64 test support
- Converted lib.rs to `#![no_std]` for embedded environment
- Removed all `#[cfg(test)]` blocks from lib.rs and pinpad/mod.rs

### Fixed
- CI compatibility improvements
- Added thumbv7em-none-eabihf target to CI toolchain

## [0.0.2] - 2026-03-12

### Added
- USB CCID class implementation with full descriptor support
- Smartcard UART driver for ISO 7816 communication
- T=0 and T=1 protocol support with proper block handling
- PIN pad functionality with touchscreen integration via FT6x06
- **Device Profile Support** (3 profiles):
  - Cherry ST-2100 (VID:0x046A PID:0x003E) - Basic reader, no PIN pad
  - Gemalto Plain (VID:0x08E6 PID:0x3437) - Basic reader, no PIN pad
  - Gemalto PINpad (VID:0x08E6 PID:0x3438) - Full PIN pad support
- SecureState machine for PIN entry workflow
- Hardware touchscreen integration for PIN entry
- Mock PIN entry mode for testing without display
- APDU builder for PIN verification commands

### Fixed
- T=1 R-block N(R) computation now correctly derived from card's N(S) sequence number per ISO 7816-3 spec, fixing duplicate data in responses
- Gemalto Plain profile corrected to use Short APDU exchange level
- CI failure incident handling and documentation

## [0.0.1] - 2026-03-08

### Added
- Initial release
- STM32F469-DISCO firmware base
- USB infrastructure setup
- Smartcard interface initialization

### Fixed
- CI setup for cargo-binutils objcopy command installation

[Unreleased]: https://github.com/yourusername/ccid-reader/compare/v0.0.7...HEAD
[0.0.7]: https://github.com/yourusername/ccid-reader/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/yourusername/ccid-reader/compare/v0.0.4...v0.0.6
[0.0.4]: https://github.com/yourusername/ccid-reader/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/yourusername/ccid-reader/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/yourusername/ccid-reader/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/yourusername/ccid-reader/releases/tag/v0.0.1
