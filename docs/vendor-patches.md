# Vendored Dependency Patches

This repository vendors three dependencies with local modifications not available upstream.

## vendor/iso14443-rs

- **Upstream**: https://github.com/Foundation-Devices/iso14443-rs
- **Version**: 0.1.0
- **License**: GPL-3.0-or-later
- **Vendored in**: commit `c6c30d0`
- **Used by**: `esp32-ccid` (via `iso14443 = { path = "../vendor/iso14443-rs" }`)

### Why vendored

The upstream crate lacks APIs needed for MFRC522 hardware workarounds and ISO-DEP session management. These patches add timeout control, frame size capping, and session lifecycle management required for reliable NFC card communication through the MFRC522's 64-byte FIFO.

### Patches

| Symbol | Purpose |
|--------|---------|
| `PcdSession` | Session-based ISO-DEP lifecycle struct. Manages activation, APDU exchange, and deactivation in a single object rather than loose function calls. |
| `try_set_timeout_ms` | Configurable timeout for ISO-DEP frame waiting. The MFRC522 hardware requires explicit timeout control that upstream doesn't expose. |
| `set_fsc` | Frame Size Card — caps the maximum frame size to match the MFRC522's 64-byte FIFO. Without this, the ISO-DEP layer attempts frames larger than the hardware can buffer. |
| `set_base_fwt_ms` | Base Frame Waiting Time — sets the minimum time to wait for a card response. Needed because the MFRC522's timing differs from typical PCD hardware. |

## vendor/mfrc522

- **Upstream**: Unknown — the Cargo.toml identifies it as `mfrc522 0.8.0` with description "Vendored MFRC522 driver — patched for esp32-ccid (timer, register access)". No original repository URL is recorded.
- **License**: Not specified in vendored Cargo.toml
- **Vendored in**: commit `c6c30d0`
- **Used by**: `esp32-ccid` (via `mfrc522 = { path = "../vendor/mfrc522" }`)

### Why vendored

Patched for ESP32-specific timer handling and register access patterns. The ESP-IDF HAL's I2C interface behaves differently from the embedded-hal implementations the original driver targets.

### Patches

- Timer and register access modifications for ESP-IDF I2C compatibility
- The vendored version uses `embedded-hal` 1.0 (the `embedded-hal-1` feature) and `heapless` 0.8

## vendor/synopsys-usb-otg

- **Type**: Git submodule
- **Upstream**: Referenced in `.gitmodules`
- **Revision**: `764bc042`
- **Used by**: STM32 firmware (via `[patch.crates-io] synopsys-usb-otg = { path = "vendor/synopsys-usb-otg" }`)

### Why vendored

The `synopsys-usb-otg` crate provides the USB OTG driver for STM32F4. The pinned revision contains fixes for USB enumeration stability that are not available in the latest crates.io release. The firmware requires release mode builds for reliable USB behavior with this driver.

### Patches

Check for local modifications:

```bash
cd vendor/synopsys-usb-otg && git log --oneline -5
```

If the submodule shows commits not on the upstream branch, those are local patches.

## Upgrade considerations

- **iso14443-rs**: Monitor https://github.com/Foundation-Devices/iso14443-rs for releases that include `PcdSession` or equivalent session management. If upstream adopts similar APIs, re-vendor and adapt.
- **mfrc522**: Without a known upstream, this crate must be maintained locally. Bug fixes should be applied directly to the vendored copy.
- **synopsys-usb-otg**: Check if newer crates.io versions fix the USB stability issues. If so, the submodule patch can be dropped in favor of the crates.io version.
- **General**: Before updating any vendored dependency, run the full test suite (`cargo test --target x86_64-unknown-linux-gnu` from root and from `esp32-ccid/`) and verify on hardware.
