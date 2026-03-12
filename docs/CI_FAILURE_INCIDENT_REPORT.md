# CI Failure Incident Report

**Date**: 2026-03-12  
**Incident**: CI pipeline failures after v0.0.2 merge  
**Last Known Good Commit**: `3d3b9de6c5e9a88550fe152a7b04032da53a84d2`  
**Fixed Commit**: `ea6870d`

---

## Executive Summary

A large feature commit (`v0.0.2: CCID firmware with PIN pad support`) introduced multiple CI-breaking issues that were **not caught before merging** because the changes weren't tested against the CI test target (`x86_64-unknown-linux-gnu`).

---

## Commit History

| Commit | Description | Status |
|--------|-------------|--------|
| `3d20049` | USB CCID firmware - initial release v0.0.1 | ✅ Working |
| `24b5833` | fix(ci): repair broken lint job | ✅ Working |
| `9e22585` | fix(ci): add explicit toolchain input | ✅ Working |
| `db9a215` | fix(release): fix heredoc syntax in YAML | ✅ Working |
| **`3d3b9de`** | **fix(ci): install cargo-binutils** | **✅ LAST KNOWN GOOD** |
| `207bcf1` → `ea6870d` | v0.0.2: PIN pad support (33 files, +8824 lines) | ❌ **BROKE CI** |

---

## Root Cause Analysis

### Issue 1: `defmt::Format` Used Unconditionally

**Location**: `src/pinpad/mod.rs:27`

```rust
// BEFORE (broken)
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub enum PinResult { ... }
```

**Problem**: `defmt` is only a dependency for ARM embedded targets:

```toml
[target.'cfg(all(target_arch = "arm", target_os = "none"))'.dependencies]
defmt = "0.3"
```

When CI runs `cargo test --target x86_64-unknown-linux-gnu`, the `defmt` crate doesn't exist.

**Fix Applied**:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(all(target_arch = "arm", target_os = "none"), derive(defmt::Format))]
pub enum PinResult { ... }
```

---

### Issue 2: `llvm-objcopy` Not Found

**Location**: CI workflow during `cargo objcopy` step

**Error**:

```
Could not find tool: objcopy
Error: Process completed with exit code 102
```

**Problem**: The original `3d3b9de` commit added `cargo install cargo-binutils` but `cargo objcopy` requires the `llvm-tools` component.

**Fix Applied** (`.github/workflows/ci.yml`):

```yaml
- name: Install cargo-binutils
  run: |
    rustup component add llvm-tools  # ADDED
    cargo install cargo-binutils
```

---

### Issue 3: `main.rs` Compiled for Host Tests

**Location**: `src/main.rs`

**Problem**: Even with `test = false` in `Cargo.toml`, the binary is still **compiled** (not tested) when running `cargo test`. Since `main.rs` uses ARM-only crates (`stm32f4xx_hal`, `defmt_rtt`, `panic_probe`, etc.), it fails for `x86_64-unknown-linux-gnu`.

**Fix Applied**:

```rust
// Added at top of main.rs
#![cfg(all(target_arch = "arm", target_os = "none"))]
```

---

### Issue 4: Test Array Size Mismatch

**Location**: `tests/integration_test.rs:16`

```rust
// BEFORE (broken)
let ccid_data: [u8; 20] = [
    30, 0x82, 0x00, 0x00, 8, 6, 0x02, 1, 0x09, 0x04, 0, 0,
    0x00, 0x20, 0x00, 0x81, 0x08,  // Only 17 elements!
];
```

**Problem**: Declared `[u8; 20]` but provided 17 elements.

**Fix Applied**:

```rust
let ccid_data: [u8; 17] = [ ... ]
```

---

### Issue 5: Display-Feature Tests Run Without Feature

**Location**: `tests/comprehensive_test.rs`

**Problem**: Tests imported `ButtonId`, `Keypad`, `TouchHandler`, and `embedded_graphics` which are only available with the `display` feature. CI tests run without this feature.

**Fix Applied**:

```rust
#[cfg(feature = "display")]
use ccid_firmware_rs::{ButtonId, Keypad, TouchHandler};
#[cfg(feature = "display")]
use embedded_graphics::prelude::*;

// ... and feature-gated all keypad/touch tests with:
#[cfg(feature = "display")]
#[test]
fn test_keypad_all_digit_buttons() { ... }
```

---

## Why This Wasn't Caught Before Merge

1. **No local testing against CI target**: The developer likely only tested with `cargo build` (ARM target) but not `cargo test --target x86_64-unknown-linux-gnu`

2. **Large monolithic commit**: 33 files, +8824 lines in a single commit makes review difficult

3. **Mixed host/embedded code**: The project structure mixes:
   - Embedded-only code (`main.rs`, `smartcard.rs`, `ccid.rs`)
   - Portable library code (`pinpad/mod.rs`, `protocol_unit.rs`)
   - Feature-gated code (`pinpad/ui.rs`)
   
   Without proper `cfg` gates, compilation fails when targeting non-ARM.

4. **Test files added without considering CI environment**: The test files assumed features and dependencies that CI doesn't have.

---

## Recommendations

| Priority | Recommendation |
|----------|----------------|
| **HIGH** | Always run `cargo test --target x86_64-unknown-linux-gnu` locally before pushing |
| **HIGH** | Add CI check that validates the lib compiles for host target |
| **MEDIUM** | Consider splitting embedded-only code into separate crate or stricter cfg gates |
| **MEDIUM** | Add pre-commit hook that runs host tests |
| **LOW** | Consider smaller, focused commits for large features |

---

## Files Modified During Fix

| File | Change |
|------|--------|
| `src/pinpad/mod.rs` | Added `cfg_attr` for conditional `defmt::Format` derive |
| `src/main.rs` | Added `#![cfg(all(target_arch = "arm", target_os = "none"))]` |
| `.github/workflows/ci.yml` | Added `rustup component add llvm-tools` |
| `tests/integration_test.rs` | Fixed array size `[u8; 20]` → `[u8; 17]` |
| `tests/comprehensive_test.rs` | Feature-gated display-dependent imports and tests |

---

## Final State

**Current HEAD**: `ea6870d`

All fixes have been applied and force-pushed. The commit now includes:

- ✅ `defmt::Format` conditionally derived
- ✅ `llvm-tools` component installed in CI
- ✅ `main.rs` cfg-gated for ARM only
- ✅ Test array size fixed
- ✅ Display-dependent tests feature-gated

---

## Lessons Learned

1. **Embedded Rust projects with host tests require careful cfg management**
   - Any code using HAL crates must be gated
   - `defmt` is target-specific
   - Test files must respect feature flags

2. **CI tests run on host, not target**
   - Always validate `cargo test --target x86_64-unknown-linux-gnu` locally
   - The `[[bin]] test = false` only skips running tests, not compilation

3. **Large commits hide bugs**
   - 33 files / 8824 lines is too large to review effectively
   - Smaller, focused PRs are easier to validate
