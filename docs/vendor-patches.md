# Dependency Patches

This repository patches one crates.io dependency via a vendor path, and one via a git fork. Only the vendor-path patch is a "vendored" dependency in the policy sense.

## Git-fork patch: synopsys-usb-otg (retired from vendor/ — #264)

- **Type**: Git dependency (fork of `stm32-rs/synopsys-usb-otg`)
- **Source**: `https://github.com/Amperstrand/synopsys-usb-otg` @ `764bc042`
- **Used by**: STM32 firmware (via `[patch.crates-io]` in root `Cargo.toml`)
- **Upstream status**: No F469 feature upstream; crates.io stuck at 0.5.0 (verified 2026-10-18)

### Fork delta (2 commits on top of upstream v0.5.0)

1. `ffc83f8` feat: add STM32F469 support — re-enables endpoints for core_id `0x0000_1200` in `src/bus.rs`
2. `764bc04` fix: suppress warnings from generated RAL code — `#![allow(unexpected_cfgs)]`, `unused_imports`, `dead_code` in `src/lib.rs`

### Lifecycle

- **Current stage**: fork/git dep (vendoring-policy class 2 — the patch can ride a fork)
- **Next stage**: upstream PR of the F469 delta (owner-gated; agents never post to stm32-rs)
- **Final stage**: plain crates.io dep when upstream releases F469 support

### Upgrade trigger

If crates.io publishes a version with STM32F469 support, drop the patch entirely and let the crates.io version resolve. Verify with the full STM32 build matrix + HIL before removing.

## Vendor-path patch: stm32f469i-disc (stranded snapshot)

- **Type**: Path patch (`[patch."https://github.com/Amperstrand/stm32f469i-disc.git"]`)
- **Used by**: STM32F469 firmware display
- **Status**: ⚠️ STRANDED SNAPSHOT — the fork's `sdio-support` branch no longer exists; this vendor copy is the sole provider. Clean path documented in `docs/VENDOR_PATCH_AUDIT.md` (#2) and issue #265.

## General

Before updating any patched dependency, run the full test suite (`cargo test --workspace --target x86_64-unknown-linux-gnu`) and verify on hardware.
