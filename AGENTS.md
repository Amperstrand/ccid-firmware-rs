# AGENTS.md — ccid-firmware-rs Project Knowledge Base

## Project Overview

**ccid-firmware-rs** is a Rust USB CCID (Integrated Circuit(s) Card Interface Device)
firmware implementing the USB CCID class specification (Rev 1.1) for smartcard
readers. It targets two MCU families with three smartcard frontends:

- **STM32F469-DISCO** — USB CCID over USB OTG FS, contact smart cards via USART2
  smartcard mode (ISO 7816-3). Default build target.
- **STM32F746-DISCO** — USB CCID over USB OTG FS, contact smart cards via GPIO
  bit-banging (no smartcard-mode USART).
- **ESP32** — Serial CCID over UART0 (GemPC Twin framing, 115200 8N2), NFC cards
  via MFRC522 over I2C (primary) or PN532 over SPI (secondary).

The firmware advertises itself on USB as one of three reference commercial
readers (Cherry SmartTerminal ST-2xxx, Gemalto IDBridge CT30, Gemalto IDBridge
K30) so that existing host drivers (pcscd, OpenSC, etc.) recognise it without
custom drivers.

Repository: https://github.com/Amperstrand/ccid-firmware-rs
License: GPL-2.0-or-later

## Workspace Layout

Root `Cargo.toml` is a pure workspace manifest (no `[package]`). The build target
defaults to `thumbv7em-none-eabihf` via `.cargo/config.toml` — this is correct
for STM32 but means ESP32 commands must be run from `firmware/esp32-ccid/` or
with an explicit target override.

```
ccid-firmware-rs/
├── Cargo.toml                  # workspace manifest, profiles, [patch] tables
├── crates/                     # shared, host-buildable libraries
│   ├── ccid-protocol/          # CCID protocol types, constants, ATR parsing
│   ├── card-interface/         # card frontend trait + PresenceState (no_std)
│   ├── ccid-core/              # CCID response builders, PPS validation (21 tests)
│   └── ccid-transport-serial/  # GemPC Twin serial framing (25 tests)
├── firmware/
│   ├── ccid-firmware/          # STM32 USB CCID firmware (default-members)
│   └── esp32-ccid/             # ESP32 serial CCID firmware (MFRC522/PN532)
├── host-tools/                 # host-side tooling
├── vendor/                     # tracked patched dependencies (see below)
│   └── synopsys-usb-otg/       # patched USB OTG driver (STM32)
├── reference/                  # osmo-ccid-firmware submodule + CCID reader specs
└── tests/hardware/             # hardware integration test procedures
```

### `[patch]` tables (root Cargo.toml)

The workspace overrides one upstream crate with a locally tracked patched
copy. This patch is **required** — the upstream version does not build:

```toml
[patch.crates-io]
synopsys-usb-otg = { path = "vendor/synopsys-usb-otg" }
```

The MFRC522 driver is NOT vendored: `esp32-ccid` consumes the canonical
`Amperstrand/mfrc522-rs` fork at `ai-experiments` rev `e9ced1e` (git dep),
the same source bolty-rs uses — the former divergent `vendor/mfrc522` copy
was removed (issue #19).

The ISO 14443 crate is also NOT vendored: `esp32-ccid` consumes the canonical
`Amperstrand/iso14443-rs` fork (`ai-experiments` branch, git dep),
the same source bolty-rs uses — the former `vendor/iso14443-rs` copy
was removed (issue #6).

### Release profile (root Cargo.toml)

```toml
[profile.release]
debug = 2           # full DWARF (probe-rs RTT location info)
opt-level = "z"     # size optimization (embedded flash constraint)
lto = true
codegen-units = 1
panic = "abort"     # no unwinding

[profile.release.package.esp32-ccid]
opt-level = "s"     # ESP32 uses "s" instead of "z"

[profile.dev]
debug = 2
opt-level = 1       # faster dev iteration
```

Release mode is **required** for reliable USB behaviour with `synopsys-usb-otg`.
Do not ship dev builds.

## MCU Profiles

### STM32 profile selection (mutually exclusive)

Defined in `firmware/ccid-firmware/Cargo.toml`. Exactly one MCU feature and one
device-profile feature must be active.

| MCU Feature | Board | Smartcard Frontend | Default? |
|---|---|---|---|
| `stm32f469` | STM32F469-DISCO | USART2 smartcard mode (hardware ISO 7816-3) | ✓ |
| `stm32f746` | STM32F746-DISCO | GPIO bit-bang (no smartcard USART) | |

```toml
[features]
default = ["stm32f469", "profile-cherry-smartterminal-st2xxx"]

# MCU target selection (mutually exclusive)
stm32f469 = ["dep:stm32f4xx-hal", "dep:stm32f469i-disc"]
stm32f746 = ["dep:stm32f7xx-hal"]
```

### USB device profiles (mutually exclusive)

Reference: `reference/CCID/readers/*.txt` (authoritative device specifications).

| Profile Feature | Device | VID:PID | PIN Pad | Default? |
|---|---|---|---|---|
| `profile-cherry-smartterminal-st2xxx` | Cherry SmartTerminal ST-2xxx | 046A:003E | ✓ Yes | ✓ |
| `profile-gemalto-idbridge-ct30` | Gemalto IDBridge CT30 | 08E6:3437 | No | |
| `profile-gemalto-idbridge-k30` | Gemalto IDBridge K30 | 08E6:3438 | No | |

> **⚠️ IMPORTANT — only Cherry ST-2xxx has PIN pad support.**
> The K30 (PID:3438) is a basic reader, virtually identical to CT30 (PID:3437).
> A prior version of the firmware falsely claimed PIN pad + LCD capabilities for
> the K30 profile; this was corrected (see `CHANGELOG.md` Unreleased). The real
> K30 has `bPINSupport=0x00`, `wLcdLayout=0x0000`, and uses TPDU exchange level
> (`dwFeatures=0x00010230`), not Short APDU (`0x00020472`).

### ESP32 backend selection

ESP32 firmware lives in `firmware/esp32-ccid/` and has its own feature set:

| Backend Feature | NFC Chip | Bus | Default? |
|---|---|---|---|
| `backend-mfrc522` | MFRC522 | I2C (M5Stack Atom Matrix) | ✓ |
| `backend-pn532` | PN532 | SPI | (secondary, still supported) |

## CI Testing Protocol

CI lives in `.github/workflows/ci.yml`. Toolchain is pinned to **Rust 1.92**.
All jobs check out with `submodules: true` (required — see Gotchas).

### Jobs

| Job | Purpose | Commands |
|---|---|---|
| `stm32-build` | Build STM32 firmware for 4 matrix entries | `cargo build --release --target thumbv7em-none-eabihf` (+ objcopy artifacts) |
| `stm32-lint` | fmt + clippy for both MCU targets | `cargo fmt --check`, `cargo clippy ... -- -D warnings` |
| `stm32-test` | Host-side workspace tests | `cargo test --workspace --target x86_64-unknown-linux-gnu` |
| `esp32-host-test` | ESP32 host-side tests (no Xtensa toolchain in CI) | `cargo test --target x86_64-unknown-linux-gnu` (from `firmware/esp32-ccid/`) |
| `iso14443-host-test` | Vendored iso14443 crate tests | `cargo test --features std --target x86_64-unknown-linux-gnu` (from `vendor/iso14443-rs/`) |

### stm32-build matrix (4 entries)

```yaml
matrix:
  include:
    - profile: profile-cherry-smartterminal-st2xxx   # default features
      features: ""
    - profile: profile-gemalto-idbridge-ct30
      features: "profile-gemalto-idbridge-ct30"
    - profile: profile-gemalto-idbridge-k30
      features: "profile-gemalto-idbridge-k30"
    - profile: stm32f746-bitbang
      features: "stm32f746,profile-cherry-smartterminal-st2xxx"
```

The first matrix entry builds with **default features** (no `--no-default-features`
override). All other entries use `--no-default-features --features <list>`.

### stm32-lint clippy runs (two passes, both must be clean)

```bash
# Pass 1 — F469 default (uses default features)
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf -- -D warnings

# Pass 2 — F746 bitbang
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf \
  --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx" -- -D warnings
```

### ⚠️ CRITICAL GOTCHA — issue #25: default features MUST include `stm32f469`

The `stm32-lint` job's first clippy pass runs with **no `--no-default-features`
flag**, which means it builds whatever `[features] default = [...]` declares.

**If `default` does not include `stm32f469`, that clippy pass fails** because
none of the MCU-specific HAL crates get pulled in and the `cfg`-gated modules
(`smartcard.rs`, USB PHY reset block, etc.) have nothing to compile against.

**Rule:** the `default` feature set in `firmware/ccid-firmware/Cargo.toml` MUST
always include exactly one MCU feature (`stm32f469`) and exactly one profile
feature. The current correct default is:

```toml
default = ["stm32f469", "profile-cherry-smartterminal-st2xxx"]
```

Do not change `default` to `[]` or to an ESP32/MFRC522 combination — CI clippy
will break. Issue #25 documents this exact regression.

### Reproducing CI locally

```bash
# Full STM32 host test suite (fast, no hardware)
cargo test --workspace --target x86_64-unknown-linux-gnu

# STM32 F469 default build (what CI ships as the Cherry profile)
cargo build --release --target thumbv7em-none-eabihf

# STM32 F469 default clippy (CI gate)
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf -- -D warnings

# STM32 F746 bitbang build
cargo build --release --target thumbv7em-none-eabihf \
  --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx"

# STM32 F746 bitbang clippy (CI gate)
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf \
  --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx" -- -D warnings

# fmt check (CI gate)
cargo fmt --check
```

## USB PHY Reset Pattern (issue #22)

### Problem

After flashing with `st-flash`, the chip performs a **soft reset** (SYSRESETREQ)
rather than a full power-on reset. The USB OTG FS peripheral retains stale PHY
state across this soft reset, which prevents USB re-enumeration on the next boot.
Symptom: the reader does not appear on the USB bus after `st-flash write ...`
until the board is physically power-cycled.

This affects both STM32F469 and STM32F746 builds.

### Fix (proven pattern, sourced from the microfips project)

> **amp-recovery note (T13):** the shared `amp-recovery` crate exposes
> `reset_usb_otg_phy()` behind `stm32f4`/`stm32f7` features, but at rev
> `67ceee1` those features do not compile (the crate references
> `stm32f4xx-hal`/`stm32f7xx-hal`/`cortex-m` without declaring them as
> dependencies — E0433). The inline sequences in `main.rs` therefore stay
> authoritative, marked `TODO(amp-recovery)`. Revisit once upstream fixes the
> feature wiring. `InitRecoveryTracker` from the same crate IS consumed by
> `esp32-ccid` (no feature flags needed).

The fix is implemented inline in `firmware/ccid-firmware/src/main.rs`, in the
block titled **"USB OTG FS PHY reset (fix for issue #22)"** (search for that
comment). The sequence runs early in `#[entry] fn main()`, before
`USB::new(...)` constructs the OTG bus. It is `cfg`-gated per MCU feature.

The sequence, in order:

1. **Disable USB OTG FS clock** — `RCC.AHB2ENR.OTGFSEN = 0`, wait ~100 cycles.
2. **Re-enable the clock** — `RCC.AHB2ENR.OTGFSEN = 1`.
3. **Assert peripheral reset** — `RCC.AHB2RSTR.OTGFSRST = 1`, wait ~100 cycles.
4. **Deassert peripheral reset** — `RCC.AHB2RSTR.OTGFSRST = 0`, wait ~100 cycles.
5. **Wait for AHB idle** — poll `GRSTCTL.AHBIDL` (bit 31) at
   `USB_OTG_FS_GLOBAL` base `0x5000_0000` + offset `0x010`, with a 100 000-iteration
   timeout.
6. **Core soft reset** — write `GRSTCTL.CSRST = 1` (bit 0, self-clearing), poll
   until it clears (100 000-iteration timeout).
7. **PHY power cycle** — write `GCCFG = 0` (offset `0x038`), wait ~100 cycles,
   then write `GCCFG.PWRDWN = 1` (bit 16).

The register addresses are raw (`0x5000_0000usize as *mut u32` + offset) because
this runs before the HAL's `USB` abstraction is constructed. All access is
`unsafe { ... read_volatile / write_volatile ... }`.

```rust
// Sketch — see main.rs for the authoritative implementation.
#[cfg(feature = "stm32f469")]
{
    unsafe {
        let rcc = &*stm32f4xx_hal::pac::RCC::ptr();
        rcc.ahb2enr().modify(|_, w| w.otgfsen().clear_bit());
        cortex_m::asm::delay(100);
        rcc.ahb2enr().modify(|_, w| w.otgfsen().set_bit());
        rcc.ahb2rstr().modify(|_, w| w.otgfsrst().set_bit());
        cortex_m::asm::delay(100);
        rcc.ahb2rstr().modify(|_, w| w.otgfsrst().clear_bit());
        cortex_m::asm::delay(100);

        let otg_global = 0x5000_0000usize as *mut u32;
        // wait AHB idle
        let mut timeout = 100_000u32;
        while otg_global.add(0x010 / 4).read_volatile() & (1 << 31) == 0 {
            timeout -= 1;
            if timeout == 0 { break; }
        }
        // core soft reset (self-clearing)
        otg_global.add(0x010 / 4).write_volatile(1);
        timeout = 100_000u32;
        while otg_global.add(0x010 / 4).read_volatile() & 1 != 0 {
            timeout -= 1;
            if timeout == 0 { break; }
        }
        // PHY power cycle (GCCFG.PWRDWN, bit 16)
        otg_global.add(0x038 / 4).write_volatile(0);
        cortex_m::asm::delay(100);
        otg_global.add(0x038 / 4).write_volatile(1 << 16);
    }
    defmt::info!("USB PHY pre-init reset");
}
```

The F746 build has an analogous block with the same sequence using
`stm32f7xx_hal::pac::RCC`. Both blocks must stay in sync.

### When this matters

- **Always**, on any boot path. The reset is cheap and idempotent.
- **Especially** after `st-flash write` (soft-reset flash path).
- Not needed after `probe-rs run` (which uses a different reset strategy), but
  running it anyway is harmless.

## ESP32 Stack Size (issue #21)

The ESP32 main task stack is sized via `sdkconfig.defaults` in
`firmware/esp32-ccid/`. The correct Kconfig symbol is:

```
CONFIG_MAIN_TASK_STACK_SIZE=16384
```

> **⚠️ Do NOT use `CONFIG_ESP_MAIN_TASK_STACK_SIZE`.**
> That symbol does not exist in current ESP-IDF and will silently be ignored,
> leaving the stack at the default (typically 3–4 KB), which is too small for
> the MFRC522 + CCID handler path and causes a stack overflow / panic on boot.
>
> The correct symbol is `CONFIG_MAIN_TASK_STACK_SIZE`. Issue #21 fixed this.

If you see an ESP32 boot panic with a stack-overflow backtrace pointing into
the CCID handler or MFRC522 driver, verify `CONFIG_MAIN_TASK_STACK_SIZE` is set
and large enough (≥ 12 KB; 16 KB is the recommended value).

## Known Gotchas

### 1. Submodule init is required

CI checks out with `submodules: true`. Locally, after cloning or pulling a
branch that touches `reference/osmo-ccid-firmware/` or any `vendor/` crate:

```bash
git submodule update --init --recursive
```

Without this, builds fail on missing `[patch]` paths and missing reference
spec files.

### 2. st-flash soft reset leaves stale USB PHY state (issue #22)

See **USB PHY Reset Pattern** above. After `st-flash write`, the OTG FS PHY
retains stale state and the device will not enumerate without the in-firmware
PHY reset sequence. If you remove or break that sequence, flashing via st-flash
will appear to succeed but the reader will not appear on USB.

### 3. espflash DTR/RTS wedge — physical replug required (issue #12)

After `espflash flash ...` on the M5Stack Atom Matrix (FTDI FT232 bridge), the
FTDI chip wedges itself via DTR/RTS toggling. The serial port disappears and
no further flashes or host-side `pcscd` communication work until the ESP32 board
is **physically unplugged and replugged** from USB.

This is a host-side FTDI driver quirk, not a firmware bug. Documented in
`CHANGELOG.md` [0.1.0] notes. Workaround: always physically replug the ESP32
board after flashing before expecting the serial CCID reader to be visible to
`pcscd`.

### 4. Default features must include `stm32f469` (issue #25)

See the CI section. Changing `default = [...]` to omit `stm32f469` breaks the
`stm32-lint` clippy pass.

### 5. ESP32 stack Kconfig symbol name (issue #21)

See **ESP32 Stack Size** above. Use `CONFIG_MAIN_TASK_STACK_SIZE`, not
`CONFIG_ESP_MAIN_TASK_STACK_SIZE`.

### 6. Release mode is mandatory for USB stability

`synopsys-usb-otg` is timing-sensitive. Dev builds (`opt-level = 1`) exhibit
unreliable USB enumeration and dropped transfers. Always ship
`cargo build --release`.

### 7. STM32F746 bitbang path is GPIO-driven, not USART

The F746 build has no smartcard-mode USART, so ISO 7816-3 is implemented in
software via `smartcard_bitbang.rs`. The F746 card clock was raised from 1 MHz
to 5 MHz (ISO 7816 maximum) — see `CHANGELOG.md` [0.1.1]. Hardware-verified at
74.4 ms average round-trip on a ComSign eID T=1 card.

### 8. F469 SRAM is capped at 256 KB

`memory.x` configures SRAM as 256 KB, not the documented 320 KB. The full 320 KB
causes a HardFault on boot (see `CHANGELOG.md` [0.0.4]). Do not "fix" this back
to 320 KB without hardware verification.

## Build Commands

### Prerequisites (one-time)

```bash
# Rust + STM32 target
rustup target add thumbv7em-none-eabihf

# ARM binutils (for objcopy)
sudo apt-get install binutils-arm-none-eabi      # Debian/Ubuntu
# brew install arm-none-eabi-binutils             # macOS

# Flashing tools (pick one)
cargo install probe-rs --features cli             # recommended
sudo apt-get install stlink-tools                 # st-flash alternative

# ESP32 (only if working on esp32-ccid)
rustup target add xtensa-esp32-espidf
cargo install espup espflash
espup install
. ~/export-esp.sh                                 # source before every ESP32 build
```

### Host tests (no hardware required)

```bash
# Full STM32 workspace host tests (matches CI stm32-test job)
cargo test --workspace --target x86_64-unknown-linux-gnu

# ESP32 host tests (matches CI esp32-host-test job)
cd firmware/esp32-ccid
cargo test --target x86_64-unknown-linux-gnu

# Vendored iso14443 host tests (matches CI iso14443-host-test job)
cd vendor/iso14443-rs
cargo test --features std --target x86_64-unknown-linux-gnu
```

### STM32 build

```bash
# Default — Cherry SmartTerminal ST-2xxx on STM32F469
cargo build --release --target thumbv7em-none-eabihf

# Gemalto CT30 profile
cargo build --release --target thumbv7em-none-eabihf \
  --no-default-features --features profile-gemalto-idbridge-ct30

# Gemalto K30 profile
cargo build --release --target thumbv7em-none-eabihf \
  --no-default-features --features profile-gemalto-idbridge-k30

# STM32F746 bitbang + Cherry profile (matches CI matrix)
cargo build --release --target thumbv7em-none-eabihf \
  --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx"
```

Binary location (all profiles):
`target/thumbv7em-none-eabihf/release/ccid-firmware`

### STM32 binary conversion + flashing

```bash
# ELF → .bin
arm-none-eabi-objcopy -O binary \
  target/thumbv7em-none-eabihf/release/ccid-firmware \
  ccid-firmware.bin
sha256sum ccid-firmware.bin > ccid-firmware.bin.sha256

# Flash via probe-rs (recommended — runs from ELF, resets cleanly)
probe-rs run --chip STM32F469NI target/thumbv7em-none-eabihf/release/ccid-firmware

# Flash via st-flash (remember the PHY-reset gotcha after this)
st-flash write ccid-firmware.bin 0x8000000
```

### ESP32 build

Run from `firmware/esp32-ccid/` (the workspace default target is STM32 — ESP32
commands must override or be run from the ESP32 crate dir).

```bash
cd firmware/esp32-ccid
. ~/export-esp.sh

# Default — MFRC522 backend
cargo +esp build --release

# Explicit MFRC522
cargo +esp build --release --features backend-mfrc522

# PN532 backend
cargo +esp build --release --no-default-features --features backend-pn532
```

Binary location: `target/xtensa-esp32-espidf/release/esp32-ccid`

```bash
# Flash (then physically replug the board — see Gotcha #3)
espflash flash --port <serial-port> target/xtensa-esp32-espidf/release/esp32-ccid
```

### Linting (CI parity)

```bash
cargo fmt --check
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf -- -D warnings
RUSTFLAGS="-D warnings" cargo clippy --release --target thumbv7em-none-eabihf \
  --no-default-features --features "stm32f746,profile-cherry-smartterminal-st2xxx" -- -D warnings
```

## Hardware Pinout (STM32F469)

Authoritative source: `PINOUT.md`. Summary:

### Smartcard interface (ISO 7816)

| MCU Pin | Signal | Direction | Notes |
|---|---|---|---|
| `PA2` | `I/O` | Bidirectional | `USART2_TX` smartcard mode, AF7, open-drain, pull-up |
| `PA4` | `CLK` | Output | `USART2_CK`, AF7, push-pull |
| `PG10` | `RST` | Output | Card reset control (active LOW) |
| `PC5` | `PWR` | Output | Card supply gate (`LOW = ON`) |
| `PC2` | `PRES` | Input | Card detect (`HIGH = card present`) |

### USB interface

| MCU Pin | Signal | Notes |
|---|---|---|
| `PA11` | `USB_DM` | OTG FS data- |
| `PA12` | `USB_DP` | OTG FS data+ |

### F746 bitbang pins (differ from F469)

| MCU Pin | Signal | Notes |
|---|---|---|
| `PI0` | `I/O` | Open-drain, pull-up, High speed |
| `PF6` | `CLK` | Push-pull, Very High speed |
| `PI2` | `RST` | Push-pull, active HIGH |
| `PF10` | `PRES` | Floating input |
| `PF7` | `PWR` | Push-pull, active LOW = ON |
| `PK3` | Backlight | Push-pull (display) |

## Sibling-Repo Improvement Pass (August 2026, issues #28–#32)

Patterns sourced from the `gm65-scanner` project (same STM32F469I-DISCO board,
same HAL fork) and the wider Amperstrand STM32 ecosystem.

### New Modules

| Module | Location | Purpose |
|--------|----------|---------|
| DWT Watchdog | `amp-dwt-watchdog` git dep (amp-embedded-common, rev `67ceee1`), re-exported as `ccid_firmware_rs::dwt_watchdog` | Cycle-counter-based wall-clock timeouts (ARM DWT CYCCNT). Replaces iteration-based polling. 14 host tests (in the canonical crate). Local module removed. |
| Diagnostics | `amp-diagnostics` git dep (amp-embedded-common, rev `67ceee1`), re-exported from `ccid-core/src/diagnostics.rs` | Runtime counter struct (apdu_tx/rx, nak, error, reinit, card_present, uptime). 28-byte LE serialization. Tests incl. byte-exact golden live in the canonical crate. |
| SmartcardConfig | `firmware/ccid-firmware/src/smartcard_common.rs` | Replaces 10 hardcoded `const SC_*` with a configurable struct. Values unchanged. |
| Self-Healing | `firmware/ccid-firmware/src/main.rs` (SmartcardWrapper), `firmware/esp32-ccid/src/mfrc522_driver.rs` | Re-init peripheral after 3 consecutive failures. `reinit_count` tracked. |
| Escape 0xD0 | `firmware/ccid-firmware/src/ccid_core.rs`, `firmware/esp32-ccid/src/ccid_handler.rs` | Vendor-neutral CCID Escape diagnostic query. Payload `[0xD0]` → 28-byte Diagnostics struct. |
| HIL Harness | `tests/hardware/labgrid/` | Pytest SSH-based HIL tests. 6 tests: USB enum, pcscd, ATR, APDU relay, pinpad. |

### SmartcardDriver Trait

`firmware/ccid-firmware/src/driver.rs` — added default `fn diagnostics()` method.
SmartcardWrapper (F469) overrides to return `reinit_count` + `card_present`.

### NfcDriver Trait

`firmware/esp32-ccid/src/nfc.rs` — added default `fn reinit_count()` method.
MFRC522 driver overrides to return actual count.

### HIL Testbed

The STM32F469I-DISCO on `192.168.13.208` is managed via labgrid (coordinator on
`.221:20408`). Security: ufw active (SSH + labgrid from `.221` only), SSH key-only.

Run HIL tests:
```bash
pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil --ssh-host=192.168.13.208
```

### libccid Configuration

To use the Escape 0xD0 diagnostic query from the host, `ifdDriverOptions` must be
set to `0x0001` in `/usr/lib/pcsc/drivers/ifd-ccid.bundle/Contents/Info.plist` on
the host running pcscd. This enables `FEATURE_CCID_ESC_COMMAND`.

## Recent Fixes

| # | Area | Summary |
|---|---|---|
| **#25** | CI | Default features in `firmware/ccid-firmware/Cargo.toml` must include `stm32f469`. The `stm32-lint` clippy pass builds with default features (no `--no-default-features`), so dropping `stm32f469` from `default` breaks CI. Current correct default: `["stm32f469", "profile-cherry-smartterminal-st2xxx"]`. |
| **#23** | HAL | `stm32f4xx-hal` dependency bumped to the Amperstrand fork pinned at rev `05d999d600d457f99aeb23ff93275d2c8f998908` (features `stm32f469`, `usb_fs`, `framebuffer`). The fork carries SDIO and USB patches not yet upstream. |
| **#22** | USB PHY | Added the OTG FS PHY reset sequence (RCC clock cycle + peripheral reset + `GRSTCTL` core soft reset + `GCCFG.PWRDWN` power cycle) at the top of `main()` for both F469 and F746 builds. Fixes re-enumeration failure after `st-flash` soft reset. Pattern proven in the microfips project. See **USB PHY Reset Pattern** above. |
| **#21** | ESP32 stack | Renamed the stack-size Kconfig from the non-existent `CONFIG_ESP_MAIN_TASK_STACK_SIZE` to the correct `CONFIG_MAIN_TASK_STACK_SIZE` in `firmware/esp32-ccid/sdkconfig.defaults`. Without this the ESP32 main task ran at the default ~3–4 KB and overflowed inside the CCID/MFRC522 path. |

## Hardware Verification History

### 2026-05 (CHANGELOG [0.1.1])
- **STM32F746-DISCO** (Cherry ST-2xxx USB CCID): 74.4 ms avg round-trip,
  ComSign eID T=1 contact card. F746 card clock raised 1 → 5 MHz.
- Both F746 and F469 firmware builds verified clean.
- ESP32 hardware testing pending (M5Stack Atom disconnected).

### 2026-04-24 (CHANGELOG [0.1.0])
- **ESP32 + MFRC522** (GemPC Twin serial): `pcscd` detects reader, NFC card responds.
  - Card: NXP P71 SmartMX3 P71D320 JCOP4 JavaCard
  - ATR: `3B 85 80 01 80 73 C8 21 10 0E` (TCK correct)
  - Reader: `GemPCTwin serial 00 00`
- **STM32 + Specter DIY Shield** (Cherry ST-2xxx USB CCID): `pcscd` detects reader,
  contact card responds.
  - Card: ComSign eID (T=1, IFSC=254)
  - ATR: `3B D5 18 FF 81 91 FE 1F C3 80 73 C8 21 10 0A` (TCK correct)
  - Reader: `Cherry GmbH SmartTerminal ST-2xxx (ST2XXX-001) 02 00`
- Both readers verified **simultaneously** on the same host.
- All host tests pass: STM32 82/82, ESP32 75/75, iso14443 52/52.

## References

- USB CCID Specification Rev 1.1 — `docs/CCID_SPEC_AUDIT.md`, `docs/AUDIT_PLAN.md`
- ISO 7816-3 (smartcard electrical/protocol)
- Reference device specs — `reference/CCID/readers/*.txt` (authoritative)
- osmo-ccid-firmware (protocol reference) — `reference/osmo-ccid-firmware/` (submodule)
- Specifications index — `docs/SPECIFICATIONS.md`
- Hardware validation procedures — `tests/hardware/README.md`
- stm32f4xx-hal (Amperstrand fork): https://github.com/Amperstrand/stm32f4xx-hal
- probe-rs: https://probe.rs

## Serial Performance Notes (issue #51/#51-closure, 2026-08-30)

Measured decomposition of the ~16ms APDU round-trip (vs ACR1252 ~2ms), via
byte-level serial probing on the rig (method in .omo evidence, session
amperstrand-nfc-mcu-dedup):

- Wire time @115200 8N2 (command + echo + response) ≈ 5-6ms.
- Firmware gap (CCID handling + MFRC522 I2C @100kHz + card I/O) ≈ 10ms —
  the dominant term and the real optimization lever.
- **Untried zero-firmware host tweak**: FTDI `latency_timer` is 4 — setting
  it to 1 (`/sys/bus/usb-serial/devices/ttyUSB0/latency_timer`) removes up
  to ~4ms of USB batching latency. Try this FIRST before any firmware work.
- Baud >115200 is host-blocked: libccidtwin hardcodes `cfsetspeed(B115200)`
  and reader.conf has no speed knob; a local ccid fork would buy ~4.6ms at
  the cost of permanent host-fork maintenance — rejected. #54 tracks the
  upstream change needed (speed knob + SIMPro2-style escape negotiation;
  firmware side is ready: `UartDriver::change_baudrate()` + escape dispatch).
