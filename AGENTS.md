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

> **amp-embedded-common dissolved (2026-08-31):** the T13 extraction into
> `amp-embedded-common` was reversed per the existential necessity audit
> (`.omo/evidence/amp-necessity-audit.md`) — the repo had exactly one legal
> consumer (this one) and its crates were parallel discovery rather than
> library-sized work. `dwt_watchdog`, `diagnostics`, and
> `InitRecoveryTracker` are restored in-repo behind their historical paths;
> these inline USB-PHY reset sequences stay deliberately (see the
> `Kept inline deliberately` comments in `main.rs`). The only surviving
> component, the reusable CI workflow, lives at org level in
> `Amperstrand/.github`. The repo itself is archived (read-only, never deleted).

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

## ESP32-C3 Nucula Board — USB Port Lifecycle (CRITICAL)

The nucula's USB-Serial/JTAG is a **composite device** (CDC serial + JTAG on one USB port). Three failure modes that WILL happen if you're not careful:

### ⚠️ Failure mode 1: Port contention

When firmware with console output is running, the CDC endpoint is actively streaming. If any process (bench logger, test fixture, `tail -f`, another terminal) holds `/dev/ttyACM*`, **esptool cannot open the port** — it hangs or gets "device reports readiness to read but returned no data".

**Prevention**:
```bash
# ALWAYS kill port holders before flashing
fuser -k /dev/ttyACM* 2>/dev/null
pkill -9 -f nucula_logger.py
sleep 2  # let the OS actually release the port
```

### ⚠️ Failure mode 2: Partition table mismatch

Flashing only the app (at 0x40000) while the board has the wallet firmware's partition table (factory app at 0x30000) means the bootloader loads the WRONG binary or nothing at all. Symptoms: silent board, old firmware running, "M1" probes appearing from a binary you flashed 30 minutes ago.

**Prevention**: ALWAYS flash bootloader + partition table + app together:
```bash
esptool write-flash 0x0 <bootloader> 0x8000 <partition-table> 0x40000 <app>
```

### ⚠️ Failure mode 3: USB peripheral state corruption

After a full chip erase or certain flash sequences, the C3's USB peripheral stops responding to software reset (DTR/RTS toggle). **Only a hard power cycle (USB replug) recovers.** No software fix.

**Prevention**: Avoid `erase-flash` unless truly needed. If you must, expect to replug afterward.

### ⚠️ Failure mode 4: silently-dropped reset → stale firmware (CRITICAL for test integrity)

esptool's post-flash "hard reset" (RTS control request to the USB-JTAG) is
**intermittently dropped**: the flash log says "Hash of data verified. Hard
resetting via RTS pin..." but the chip never resets and the OLD firmware keeps
running. Bench proof (2026-10-07): health counter continued 52→124 across a
"successful" flash; the "new firmware" test silently exercised stale code.
Raw `setRTS()` toggles on an open port fail the same way.

**Prevention** — never trust the flash log alone; PROVE what booted:

1. **FWID markers**: every Rust firmware logs `FWID <name> rev=<git> build=<ts>`
   as its first console line (build.rs stamps `FW_GIT_REV`/`FW_BUILD_TS`).
2. **`board.flash_and_boot()`** (tests/hardware/nucula/board.py) runs the full
   protocol: flash → verify expected marker (FWID / wallet prompt) → if the
   reset was dropped: JTAG reset (`openocd init; reset run; shutdown`) →
   verify → if the chip latched in download mode (`boot:0x5`): esptool
   `flash-id --after hard-reset` round-trip → verify → raise with console
   evidence.
3. **Pre/post-test checklist**: `pretest_check()` (port exists, no port
   holders, esptool responsive) gates every HIL session; the session teardown
   runs `ensure_responsive()` and falls back to `restore_known_good()` (wallet
   flash) if the board is wedged.

Note: the FWID line can print during the USB re-enumeration window and be
lost — the ladder's retry handles that; periodic markers (`health[1..2]`,
low counters) are an additional fresh-boot signal.

### ⚠️ Failure mode 5: bootloader offsets differ by chip family + overwriting unknown firmware without a backup (m5stick incident, 2026-10-08)

The bench M5Stack (Hades2001 USB-serial, classic ESP32) boot-looped with
`flash read err, 1000 / ets_main.c 371` after a manual full-set flash.
ROOT CAUSE (found next morning, device fully recovered): the bootloader
was written to **0x0** — the ESP32-**C3** offset. Classic ESP32 boots its
second-stage bootloader from **0x1000**. Five "recovery attempts" failed
because they all repeated the same wrong offset while patching header
bytes (DIO/20MHz) — when N recovery attempts fail, re-derive the basics
instead of tuning guesses.

**Bootloader flash offsets (memorize or check before every manual flash):**

| Chip | Bootloader offset | Partition table | Typical app |
|---|---|---|---|
| ESP32 (classic, xtensa) | **0x1000** | 0x8000 | per table (default factory: 0x30000) |
| ESP32-C3 / S3 / C2 | **0x0** | 0x8000 | per table (our OTA table: 0x40000) |

The app must land where the partition table you ACTUALLY flashed points
(read it from the boot log — `boot: 2 factory factory app 00 00 00030000`
means factory@0x30000). build.sh --flash handles this; manual esptool
invocations are where the offset bugs creep in.

Recovery outcome: bootloader@0x1000 + default PT@0x8000 + app@0x30000 →
esp32-ccid MFRC522 firmware boots, pcscd enumerates it as
`GemPCTwin serial` on /dev/ttyUSB2, CCID GetSlotStatus answers correctly.

**Prevention**: before overwriting unknown firmware on ANY bench device:

```bash
esptool --chip esp32 -p PORT read-flash 0x0 0x400000 device-backup.bin
```

One minute of backup versus a soft-bricked board nobody can restore
unattended. This belongs in the HIL pretest checklist for shared rigs.

### ⚠️ Boot message loss during USB-CDC re-enumeration

After flashing, the USB device disconnects and reconnects. The first 1-2 seconds of boot output (including boot banners, VEN cycle logs, early probe results) are lost. This is NOT a firmware bug.

**Prevention**: Use `ConsoleCapture` (tests/hardware/nucula/console.py) which polls the by-id path at 50ms intervals. Add boot markers to distinguish firmware versions.

### Board state decision tree

```
Board silent after flash?
├── Check port exists: ls /dev/serial/by-id/usb-Espressif*
│   └── No port → USB replug needed (failure mode 3)
├── Check what's running: is old firmware's console output visible?
│   └── Old firmware → partition table mismatch (failure mode 2)
├── Try esptool flash-id
│   └── Hangs → port contention (failure mode 1)
└── Try RTS reset
    └── Still silent → USB replug needed
```

### Recovery: the nuclear option

```bash
# Kill everything
fuser -k /dev/ttyACM* 2>/dev/null
pkill -9 -f "esptool|nucula_logger|python.*serial"

# Wait for USB to settle
sleep 5

# Full flash from known-good wallet firmware
cd /tmp/opencode/nucula-fw/build-551
esptool --chip esp32c3 -p /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:72:9A:50:18-if00 \
  --baud 460800 --after hard-reset write-flash \
  0x0 bootloader/bootloader.bin \
  0x8000 partition_table/partition-table.bin \
  0x30000 nucula.bin

# Verify: console shows "nucula>" prompt
# If still silent → try JTAG reset (below), then USB replug as last resort
```

### JTAG reset — the software-only recovery (no replug needed)

The USB-JTAG peripheral has a JTAG path **separate from the CDC serial path**. Even when the CDC is stuck, JTAG often still works. This is the first recovery to try before reaching for the USB cable:

```bash
# One-liner: connect via JTAG, issue system reset, resume, disconnect
/opt/espressif/openocd-esp32/bin/openocd \
  -f board/esp32c3-builtin.cfg \
  -c "init; reset run; shutdown"
```

If the board responds to JTAG (you see "JTAG tap: esp32c3.tap0 tap/device found"), the reset was issued. Wait 2–3 seconds for the board to reboot, then try esptool or console again.

**Recovery escalation ladder** (try in order):
```
Board stuck / unresponsive?
├── 1. Kill port holders: fuser -k /dev/ttyACM*; pkill -9 -f nucula_logger
├── 2. RTS/DTR reset: python3 -c "import serial,time; s=serial.Serial(PORT,115200); s.setRTS(True); time.sleep(0.2); s.setRTS(False); s.close()"
├── 3. esptool reset: esptool --chip esp32c3 -p PORT --after hard-reset flash-id
├── 4. JTAG reset: openocd -f board/esp32c3-builtin.cfg -c "init; reset run; shutdown"
└── 5. USB replug (nuclear — last resort)
```

Steps 1–3 fix port contention (failure mode 1).
Step 4 fixes USB peripheral corruption (failure mode 3) WITHOUT replug.
Step 5 fixes everything but requires physical access.

### Labgrid test framework (tests/hardware/nucula/)

Automated flash + test cycle — eliminates manual flash-and-pray:

```bash
# Run all nucula HIL tests
pytest tests/hardware/nucula/test_nucula.py -v --hil

# Just check board is responsive
pytest tests/hardware/nucula/test_nucula.py -v --hil -k test_board_responsive

# Flash wallet firmware and verify NCI init
pytest tests/hardware/nucula/test_nucula.py -v --hil -k test_wallet
```

Components:
- `board.py` — board metadata (pins, flash offsets, I2C devices) + esptool with port cleanup and retry logic
- `console.py` — USB-CDC capture with fast re-enumeration handling + boot markers
- `conftest.py` — pytest fixtures
- `test_nucula.py` — test pyramid: responsive → wallet boots → I2C scan → our firmware ACKs → NCI init → sustained

### esptool v5.3.1 gotcha: global options before subcommand

`--after` is a **global** option and must come BEFORE `write-flash`:
```bash
# CORRECT: esptool ... --after hard-reset write-flash ...
# WRONG:   esptool ... write-flash --after hard-reset ...  ← "No such option"
```

This bit us because the board's `run_esptool()` method originally placed
`--after` after the subcommand. If you get "No such option '--after'",
check the option ordering.

## PN7160 ACK Window (issue #63 root cause, bench-proven 2026-10-08)

The PN7160's I2C slave only ACKs when the host talks to it **immediately
after VEN rise and keeps talking**. Probing 5s after the VEN cycle = the
chip is permanently mute (NAK forever); probing 50ms after and proceeding
straight into CORE_RESET/CORE_INIT without pause = the chip responds and
stays alive. The wallet firmware always did the latter (nci_init probes
at +50ms); our firmware always probed seconds later — every earlier
theory (ISR priorities, sdkconfig diffs, bus priming, address straps,
build system) was a red herring.

Proven via C-control bisection (idf.py + IDF 5.5.1 builds of the same
sequence): v1-v3 NAK (build system exonerated — issue #83 disproved),
v4-v9 = wallet's own functions from a minimal main ACK (wifi/nvs/console/
keypad all unnecessary), v10 = the timing test that isolated the window.

The fix lives in `pn7160_i2c.rs` (v10raw-verified init: bus first, raw
gpio_config IRQ+ISR machinery, single clean VEN cycle, probe immediately)
and `pn7160_bringup.rs` (VEN re-cycle + immediate probe on NAK; ladder
runs without pause on ACK). `pn7160_v10raw.rs` is the known-good
pure-syscall reference binary — flash it first when in doubt.

**Init-order hazard**: `gpio_install_isr_service` BEFORE
`i2c_new_master_bus` hard-hangs the app (interrupt allocation deadlock,
C-control v5/v6 + Rust both). The bus must be created before any ISR
service installation.

**IRQ ISR contract**: any ISR handler on the PN7160's IRQ pin must
quench the level interrupt itself (`gpio_intr_disable`) — the chip holds
IRQ high until read; a no-op handler = interrupt storm that starves the
console/USB the moment the chip comes alive.

**Console starvation**: the NCI ladder's `Ets::delay_us` busy-wait (IRQ
polling) starves the USB console task during the ladder — console output
dies while the app + chip keep working (verify via GDB-over-JTAG, which
shows live transport state). Convert ladder waits to FreeRtos delays.

## ESP32-C3 nucula Build Flow (issues #63/#64, ai-legion)

The nucula (ESP32-C3 + PN7160) firmware builds with the nightly toolchain
(`-Z build-std` in `firmware/esp32-ccid/.cargo/config.toml`; no rustup target
needed). Build from `firmware/esp32-ccid/`:

```bash
source ~/.cargo/env && source ~/export-esp.sh
export RUSTUP_TOOLCHAIN=nightly
export ESP_IDF_SDKCONFIG="$PWD/sdkconfig.full"     # NOT plain SDKCONFIG!
cargo build --target riscv32imc-esp-espidf --no-default-features \
  --features pn7160-bringup,pn7160-verdict-b
```

### ⚠️ Global target-dir: set `CARGO_WORKSPACE_DIR` or the IDF pin is ignored

This machine's `~/.cargo/config.toml` pins `target-dir = /root/.cargo-target`
(shared by every checkout). The esp-idf-sys build script locates the root
crate by popping its OUT_DIR six levels — under a shared target dir that
lands INSIDE the target dir, `cargo metadata` finds no project, and the
build silently falls back to whatever IDF the stale `.embuild` state holds.
Observed: the bench compiled **v5.2.3 all day** while the manifest said
v5.2.4 (and later v5.5.1) — the pin was never even read. CI is unaffected
(per-repo target dir), which is why CI cloned v5.2.4 correctly while the
bench diverged.

Fix (encoded in `build.sh`):

```bash
export CARGO_WORKSPACE_DIR="<repo root>"
```

Verify after building — the `esp_idf_dir` must match the manifest pin:

```bash
python3 -c "import json,glob; print(json.load(open(glob.glob('/root/.cargo-target/riscv32imc-esp-espidf/debug/build/esp-idf-sys-*/out/esp-idf-build.json')[0]))['esp_idf_dir'])"
```

Also note: changing `esp_idf_version` in Cargo.toml does NOT rerun the build
script (nothing declares rerun-if-changed on manifest metadata) —
`cargo clean -p esp-idf-sys --target <triple>` (or delete the
`build/esp-idf-sys-*` dirs) after a version bump.

### ⚠️ The env var is `ESP_IDF_SDKCONFIG`, not `SDKCONFIG`

esp-idf-sys/embuild reads **`ESP_IDF_SDKCONFIG`**. A plain `SDKCONFIG` export
is silently ignored and the build falls back to whatever sdkconfig defaults
state is cached in the shared target dir — including STALE absolute paths
from previous working copies (this machine's global
`~/.cargo/config.toml` sets `target-dir = /root/.cargo-target`, shared by
every checkout). Verify the active path after building:

```bash
python3 -c "import json,glob; print(json.load(open(glob.glob('/root/.cargo-target/riscv32imc-esp-espidf/debug/build/esp-idf-sys*/out/esp-idf-build.json')[0]))['sdkconfig'])"
```

After changing `sdkconfig.full`, delete the cmake cache or the change is
ignored:

```bash
OUT=$(ls -d /root/.cargo-target/riscv32imc-esp-espidf/debug/build/esp-idf-sys*/out | head -1)
rm -f "$OUT/build/CMakeCache.txt"
```

The partition CSV (`partitions-ota.csv`, referenced by
`CONFIG_PARTITION_TABLE_CUSTOM_FILENAME` in `sdkconfig.full`) no longer needs
manual copying: the esp-idf-sys build script injects `ESP_IDF_GLOB_*` matches
into the embuild out dir **before cmake configure** (esp-idf-sys
BUILD-OPTIONS.md). `build.sh` exports the pair; bare `cargo build` needs them
in the environment:

```bash
export ESP_IDF_GLOB_PARTCSV_BASE="$PWD"
export ESP_IDF_GLOB_PARTCSV_1="/partitions-ota.csv"
```

CMake rewrites `sdkconfig.full` in place (kconfig normalization — new symbols
appear, deprecated ones migrate). That is expected.

### Flash (esptool, elf2image required)

esptool does not auto-convert Rust ELF output — `write-flash 0x40000 <elf>`
fails with "will not fit in flash". Convert first:

```bash
esptool --chip esp32c3 elf2image --output app.bin \
  /root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid
PORT=/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:72:9A:50:18-if00
esptool --chip esp32c3 -p $PORT --baud 460800 write-flash \
  0x8000 /root/.cargo-target/riscv32imc-esp-espidf/debug/build/partition-table.bin \
  0x40000 app.bin
```

Flash the partition table (`0x8000`) whenever `partitions-ota.csv` changed —
flashing only the app leaves the old table on the board and the coredump
component reports no partition.

### Coredump decode (issue #64)

The coredump partition (`coredump, data, coredump, 0x3F0000, 0x10000` in
`partitions-ota.csv`, `CONFIG_ESP_COREDUMP_ENABLE_TO_FLASH=y` in
`sdkconfig.full`) is verified end-to-end: panic → flash write → host decode.
Read + decode with the embuild IDF copy and gdb-multiarch:

```bash
esptool --chip esp32c3 -p $PORT read-flash 0x3F0000 0x10000 coredump.bin
VENV=/root/.cargo-target/.embuild/espressif/python_env/idf5.2_py3.12_env/bin/python
$VENV /root/.cargo-target/.embuild/espressif/esp-idf/v5.2.3/components/espcoredump/espcoredump.py \
  --chip esp32c3 info_corefile --core coredump.bin --core-format raw \
  --gdb /usr/bin/gdb-multiarch \
  /root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid
```

GDB over the built-in USB-JTAG (same port) — **verified end-to-end on
ai-legion (issue #64)**: hardware breakpoint on `i2c_master_cmd_begin`
hit within one 5 s health-probe cycle with a fully symbolized backtrace
(IDF C → esp-idf-hal → `EspPn7160Transport::probe` → bring-up main) and
live argument values, target resumed cleanly.

Toolchain on ai-legion:
- openocd: Debian's `openocd` 0.12 package LACKS `target/esp32c3.cfg` and
  `interface/esp_usb_jtag.cfg` — use Espressif's build:
  `https://github.com/espressif/openocd-esp32/releases` (tarball extracted
  to `/opt/espressif/openocd-esp32`).
- gdb: `gdb-multiarch` works as the client (no riscv32-esp-elf-gdb needed —
  it also decodes coredumps, see above).

```bash
/opt/espressif/openocd-esp32/bin/openocd -f board/esp32c3-builtin.cfg > /tmp/openocd.log 2>&1 &
# Wait for the gdb server — openocd is still loading config / initializing
# USB-JTAG when the shell prompt returns; an immediate telnet connect gets
# ConnectionRefused and gdb then attaches without the required halt:
python3 - <<'PYEOF'
import socket, time, sys
for _ in range(50):
    try:
        socket.create_connection(("localhost", 3333), timeout=1).close()
        sys.exit(0)
    except OSError:
        time.sleep(0.2)
sys.exit("openocd gdb server never came up; see /tmp/openocd.log")
PYEOF
# Halt via the telnet command port BEFORE attaching gdb — attaching to a
# running target yields bogus registers (pc/sp zeros):
python3 -c "import socket,time; s=socket.create_connection(('localhost',4444)); s.sendall(b'halt\n'); time.sleep(1)"
gdb-multiarch -q -batch \
  -ex "file /root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid" \
  -ex "target remote :3333" \
  -ex "break i2c_master_cmd_begin" -ex "continue" -ex "bt" -ex "detach"
python3 -c "import socket,time; s=socket.create_connection(('localhost',4444)); s.sendall(b'resume\n')"  # leave it running
# Shut openocd down — detach/resume do NOT terminate it; a lingering server
# holds the USB-JTAG adapter and ports 3333/4444, blocking the next session:
python3 -c "import socket,time; s=socket.create_connection(('localhost',4444)); s.sendall(b'shutdown\n'); time.sleep(1)"
```

Note: small firmware fns (`probe()`) are inlined at this opt level — break on
a non-inlined callee (`i2c_master_cmd_begin`, `I2cDriver::write`) instead.
After openocd exits the USB-JTAG CDC re-enumerates; the next serial open can
block for a few seconds.

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
| DWT Watchdog | `firmware/ccid-firmware/src/dwt_watchdog.rs` | Cycle-counter-based wall-clock timeouts (ARM DWT CYCCNT). Replaces iteration-based polling. 14 host tests. |
| Diagnostics | `crates/ccid-core/src/diagnostics.rs` | Runtime counter struct (apdu_tx/rx, nak, error, reinit, card_present, uptime). 28-byte LE serialization. 12 tests (incl. byte-exact golden ported from amp-diagnostics before archival). |
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

## External posting (owner directive 2026-09-06 — CHANNEL rule)

Agents never post on non-member repos — no `gh` writes (issues, PRs,
comments, reviews, gists), not even with per-text owner sign-off; the
owner does the copy-paste into GitHub themselves. Member orgs (verify:
`gh api user/orgs`; 2026-09-06: Amperstrand, OpenTollGate, net4sats,
FreedomTechFeed) keep the existing owner-gate flow. Read the target
repo CONTRIBUTING/AI policy before drafting anything upstream.
Canonical text: lightning-playground AGENTS.md (standing rule UPDATE
2026-09-06).
