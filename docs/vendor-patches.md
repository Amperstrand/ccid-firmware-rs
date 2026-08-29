# Vendored Dependency Patches

This repository vendors one dependency with local modifications not available upstream.

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

- **iso14443-rs**: Now consumed from the canonical `Amperstrand/iso14443-rs` fork (`ai-experiments` branch, shared with bolty-rs), not vendored. The fork carries the APIs needed for MFRC522 hardware workarounds (PcdSession, timeout control, frame size capping).
- **synopsys-usb-otg**: Check if newer crates.io versions fix the USB stability issues. If so, the submodule patch can be dropped in favor of the crates.io version.
- **General**: Before updating any vendored dependency, run the full test suite (`cargo test --target x86_64-unknown-linux-gnu` from root and from `esp32-ccid/`) and verify on hardware.
