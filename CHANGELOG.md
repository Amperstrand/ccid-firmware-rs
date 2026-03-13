# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.6] - 2026-03-13

### Added
- Multi-profile CI/CD: All 3 profiles (Cherry ST-2100, Gemalto Plain, Gemalto PINpad) now built and released
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

[Unreleased]: https://github.com/yourusername/ccid-reader/compare/v0.0.6...HEAD
[0.0.6]: https://github.com/yourusername/ccid-reader/compare/v0.0.4...v0.0.6
[0.0.4]: https://github.com/yourusername/ccid-reader/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/yourusername/ccid-reader/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/yourusername/ccid-reader/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/yourusername/ccid-reader/releases/tag/v0.0.1
