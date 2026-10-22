# BOARDS-AS-DATA — board tables as const data, one home (ccid-firmware-rs), bolty consumes

**Status:** DESIGN — not implemented. Implementation is a later card (kanban `t_2f868bc4`).
**Epic:** `t_506dfe03` (one kernel, two repos; boards+matrix milestone). This design is the
data-layer leg of that milestone; it must not collide with the ACTIVE M2 mfrc522-pcd homing
lane (branch `refactor/m2-mfrc522-pcd-home` — kernel crate moves) nor the M3 presets lane
(`cargo xtask build product=X`, no `xtask/` exists in-repo yet — only `tools/`).
**Precedent:** the pn7160-nci crate already homes board data in this repo and bolty already
consumes it rev-pinned (`bolty-rs crates/bolty-pn7160/Cargo.toml` pins
`ccid-firmware-rs@0b8e33e3…`, and `bolty-rs crates/bolty-pn7160/src/lib.rs:29` re-exports
`pn7160_nci::board::{BoardPins, NUCULA}`). This design generalizes that shape from one
crate's block to the whole board.

---

## 0. WHAT / WHY

**WHAT.** Every board's wiring — pin/bus map, NFC peripheral + bus address, capability set,
OTA-partition family — becomes **const data in one kernel crate** (`crates/boards`,
proposed name, §1.4), homed in ccid-firmware-rs. Frontend constructors in the firmware
bins, product presets, the union-matrix CI, and (via a rev-pinned git dep, the pn7160-nci
precedent) bolty-rs all read the same tables.

**WHY — the copy-paste drift is real and already measured.** The m5stick Grove map alone
currently lives in **five places across two repos**:

| # | Where (path:line, 2026-10-21, main @ f60ff28f) | Form |
|---|---|---|
| 1 | `firmware/esp32-ccid/Cargo.toml:24-26` — "Grove I2C pinout differs across M5Stack boards (bolty-rs docs/lessons-learned.md B2)" | **comment quoting bolty docs** |
| 2 | `firmware/esp32-ccid/src/main.rs:641-642` — `#[cfg(feature = "board-m5stick")] let (sda_gpio_no, scl_gpio_no) = (32, 33);` | cfg pin tuple |
| 3 | `firmware/esp32-ccid/src/main.rs:26` — `compile_error!("…board-m5stick (Grove SDA=32/SCL=33)…")` | pin data inside an error string |
| 4 | `bolty-rs docs/lessons-learned.md` B2 (SDA=GPIO32/SCL=GPIO33, MFRC522 @ 0x28; m5atom 26/32) | prose lesson |
| 5 | `bolty-rs apps/bolty-esp32/Cargo.toml:18-19` board features + display/LED pin args in `src/display.rs:193-195` | bolty's own duplicate |

Rows 1–3 are three copies of the same numbers inside ONE repo. The card's test —
"adding a board touches one file" — is precisely what kills this class. The nucula pins
are likewise quadruplicated: `firmware/esp32-ccid/Cargo.toml:29-33` (comment),
`src/main.rs:645-646`, `src/pn7160_ccid.rs:125-131` (BusPins gpio4/5/6/7), and
`crates/pn7160-nci/src/board.rs:30-36` (`NUCULA` — the ONE good copy, with unit tests).

The strategy note in `docs/SHARED-CRATES.md` (scope note, t_72e1e2ed) already flags the
known gap: "board pin knowledge is copy-pasted (ccid quotes bolty docs)".

**Non-goals.**
- No runtime board auto-detection. bolty's `docs/unified-board-config.md` explores I2C-scan
  detection for standalone mode; we stay **compile-time selected** (flash budget,
  dead-code elimination, and the mutually-exclusive-feature contract that
  `main.rs:6-26` already enforces with `compile_error!`).
- No change to `crates/ccid-service` (§3.2 — the boundary is a *consumer* of this design's
  constraint, not an edit target).
- No kernel-crate moves (M2 lane owns `mfrc522-pcd`; this design adds a NEW crate only).
- No hardware bring-up in the migration PRs beyond what CI + existing bench flows already do.

---

## 1. Table schema — per-board const data

### 1.1 Two layers: data table (new crate) × constructor (firmware bin)

A board table cannot be *only* data: esp-idf-hal peripherals are **type-state tokens**
(`peripherals.pins.gpio13` is a `Gpio13<'static>` consumed by value — see
`firmware/esp32-ccid/src/m5stick_lcd.rs:413-421`, which takes `Gpio13`, `Gpio15`, … as
distinct types). No `const` table can hand out typed tokens. So the design is:

- **Layer A — `crates/boards` (the table):** pure, `no_std`, `core`-only const data:
  numbers, enums, bools. Raw GPIO numbers are the interface, exactly like today's
  `let (sda_gpio_no, scl_gpio_no) = (32, 33);` which feeds
  `pn7160_nci::hal_i2c::I2cMasterBus::new(0, sda, scl)` — `main.rs:635-660` already
  documents why raw numbers suffice there ("driver_ng claims the controller by port").
- **Layer B — constructors in the firmware bins:** per-board `fn build(peripherals) ->
  Frontend` functions that read Layer A. The LCD call site (`main.rs:687-701`) becomes
  `m5stick_lcd::init(spi2, p.gpio(B.lcd.sclk), …)` behind a thin number→token helper, or
  (pilot scope) keeps typed calls but asserts them against the table (§4 step 1).

Layer A is what bolty, presets, CI, and docs consume. Layer B stays per-repo product glue
(per the kernel-placement rule: "own in the ccid monorepo when it is product glue" —
`docs/SHARED-CRATES.md` context / ops kernel-modularity doctrine).

### 1.2 The schema (worked example = m5stick, REAL data from today's wiring)

```rust
// crates/boards/src/m5stick.rs  — COTS board, hand-written table (§2.2)
#![allow(clippy::needless_raw_strings)] // (illustrative; real file uses plain consts)

use crate::schema::*;

/// M5StickC / StickC-Plus — the ai-legion bench rig board (bolty B2:
/// "Grove I2C on SDA=GPIO32 / SCL=GPIO33 … MFRC522 at address 0x28").
pub const M5STICK: BoardDesc = BoardDesc {
    // ---- identity ----
    name: "m5stick",
    chip: Chip::Esp32,                       // → target xtensa-esp32-espidf
    variant_note: Some("StickC-Plus LCD is a per-model capability (feature `lcd`): \
        the pinned ST7789 135x240 geometry mis-drives an original StickC's ST7735S \
        (Cargo.toml:66-72, issue #275)"),

    // ---- buses: controller number + raw GPIO numbers (Layer-A interface) ----
    buses: &[
        Bus { role: BusRole::NfcFrontend, kind: BusKind::I2c {
            port: 0, sda: 32, scl: 33,       // main.rs:641-642 (today) / B2
            hz_probe: 100_000, hz_run: 400_000, // main.rs:652-671 (probe@100k, run@400k)
        }},
        Bus { role: BusRole::PmuLcd, kind: BusKind::I2c {
            port: 1, sda: 21, scl: 22,       // m5stick_lcd.rs:408-409 (AXP192 on I2C1)
            hz_probe: 0, hz_run: 400_000,
        }},
    ],

    // ---- named pin roles (LCD block; m5stick_lcd.rs:413-421 today) ----
    pins: &[
        Pin { role: "lcd_sclk", gpio: 13 },
        Pin { role: "lcd_mosi", gpio: 15 },
        Pin { role: "lcd_cs",   gpio: 5  },
        Pin { role: "lcd_dc",   gpio: 23 },
        Pin { role: "lcd_rst",  gpio: 18 },
        Pin { role: "lcd_bl",   gpio: 27 },
    ],

    // ---- NFC peripheral on the frontend bus ----
    nfc: NfcPeripheral {
        controller: NfcController::Mfrc522,
        i2c_addr: 0x28,                       // B2; probe site main.rs:664
    },

    // ---- capabilities (what presets may compose; absence = compile-time "no") ----
    caps: Caps {
        lcd:      Cap::Feature("lcd"),        // +lcd variant (build.sh m5stick-plus-lcd)
        led:      Cap::None,                  // GPIO27 is the backlight here — led.rs:4
                                               // drives the ATOM matrix on 27; conflict
                                               // encoded as exclusivity (main.rs:682-716)
        axp192_pmu: Cap::Feature("lcd"),      // rides I2C1, claimed by the LCD init
        ble:      Cap::Feature("ble"),        // build.sh m5stick-ble
        spp:      Cap::Feature("spp"),        // build.sh m5stick-spp (ESP32 BR/EDR only)
        wifi_vpcd: Cap::Feature("wifi-vpcd"), // build.sh m5stick-vpcd
        wired_contact: false,                 // no contact pads — NFC only
    },

    // ---- console + wire ----
    console: Console { kind: ConsoleKind::Uart0GemPC, baud: 115_200, stopbits: 2 },
    wire:    Wire::GemPcSerial,               // GemPC Twin emulation, UART0

    // ---- flash/OTA family (§6) ----
    ota_partition_family: OtaFamily::EspIdfOta {
        csv: "firmware/esp32-ccid/partitions-ota.csv",  // the ONLY such CSV today
        app_slot_offset: 0x40000,                        // build.sh FLASH_OFFSET (Codex #79 P1)
        bootloader_offset: 0x1000,                       // xtensa → 0x1000 (FLASH design §4.4 c7)
    },
};
```

Every number above is taken from today's wiring, not invented: Grove 32/33 and 0x28 from
`main.rs:641-642`/`:664-674` and bolty B2; probe/run speeds from `main.rs:652-671`; AXP192
I2C1 21/22 from `m5stick_lcd.rs:405-407` and its `I2cMasterBus::new(1, 21, 22)`; LCD
13/15/5/23/18/27 from the init signature `m5stick_lcd.rs:413-421` and call site
`main.rs:690-700`; GPIO27 LED/backlight exclusivity from `main.rs:682-686` and
`led.rs:4`/`:57`; console 115200 8N2 from the GemPC contract (build.sh m5stick notes,
`docs/T1_BEST_PRACTICES.md` framing context); `ota_0 @ 0x40000` from build.sh:219 and
`partitions-ota.csv`.

The full `BoardDesc` schema fields (the type definitions the example instantiates):

| field | type | why const-data |
|---|---|---|
| `name` | `&'static str` | identity in logs/CI/presets; FWID stamps |
| `chip` | `enum Chip { Esp32, Esp32c3, Stm32f469, Stm32f746 }` | selects target triple + bootloader-offset rule (§6) |
| `buses` | `&'static [Bus]` | controller port + raw GPIO numbers + speeds; mirrors how driver_ng is driven today (`main.rs:659-661`) |
| `pins` | `&'static [Pin]` | named role → GPIO; kills magic numbers at call sites |
| `nfc` | `NfcPeripheral` | controller enum + I2C addr; per-board, not per-driver |
| `caps` | `Caps` (const struct of `Cap::{Feature(&str), None, On}`) | the preset composition surface (§3.3); encodes conflicts (GPIO27) |
| `console` / `wire` | enums | host-side contract facts (GemPC 8N2 vs CDC 8N1 — the flash/bench tools already branch on this: `esp32_flash.py --stopbits`) |
| `ota_partition_family` | enum | §6 — ties the board to the flash-bundle family |

STM32 boards get the same schema with `wired_contact: true`, `nfc: None`, and
`OtaFamily::BareMetal` (§2.3) — one representation across families is the point.

### 1.3 Why const Rust structs + slices, not the alternatives

The precedent — `crates/pn7160-nci/src/board.rs` — is exactly this shape and its doc
header states the reason: "**Pure data — host-testable, no esp deps**", with tests
asserting each field (`board.rs:46-53`). Alternatives considered:

1. **Const structs/slices (chosen).** `no_std`-friendly (`core` only, no alloc), compiles
   on cortex-m (F469) AND espidf AND the host (`x86_64`) — so the union-matrix
   `host-union-check` job (`.github/workflows/union-matrix.yml:192-219`) can unit-test
   board tables with zero toolchain cost; values are visible in rustdoc; `PartialEq`
   gives drift assertions (§4 step 3) for free.
2. **Build-time codegen (build.rs).** Rejected for COTS tables: build.rs cannot see the
   workspace's other crates' data, adds a build-dep (embuild is already the esp bin's
   build-dep — a kernel crate must stay build-dep-free), and hides the data from
   reviewers. Also forbidden for the *nucula* generator specifically: fetching from
   `zeugmaster/nucula-board` at build time breaks reproducible builds and offline CI
   (the B21 empty-clock/pinned-stamp doctrine's spirit).
3. **TOML/CSV parsed at startup.** Rejected: needs alloc + a parser in the bin, spends
   flash, and moves errors from compile time to boot time — the opposite of the
   mutually-exclusive-feature contract (`main.rs:6-26`) that exists today.
4. **Type-state per board (STM32's `board_config.rs` idiom).** Kept for Layer B
   constructors only (§2.3) — type-state encodes *ownership* of HAL tokens, which is a
   constructor's job, not a table's.

Generated code is **in-scope but off-build-path** (§2.1): the nucula table is GENERATED
by an explicit `cargo xtask`-style command whose output is checked in with a provenance
header — generated *const Rust*, i.e. option 1's representation produced by a machine.

### 1.4 Crate placement and name

- Home: **`crates/boards`** in ccid-firmware-rs (kernel crate, path-dep of
  `firmware/esp32-ccid` and `firmware/ccid-firmware`, no HAL deps, `no_std`+`std` dual —
  `#![no_std]` with `extern crate std` under `cfg(test)` so host CI runs the same code).
- Per the `docs/SHARED-CRATES.md` scope note: it is repo-internal (path dep) and NOT
  listed in the kernel-crates table **until** bolty consumes it via a git dep — then it
  gains a row + drift-check entry (§3.4).
- Name candidates: `boards` (top-level claim), `board-registry`, `ccid-boards`. The
  pn7160-nci precedent keeps block-scoped names (`board.rs` inside the crate); a
  workspace-level claim wants the short name. **Open decision O-2**, default `boards`.

---

## 2. Source of truth per family

Three families with three different epistemologies — that IS the design; pretending one
source fits all is how the current drift happened.

### 2.1 nucula — GENERATED from `zeugmaster/nucula-board`, never hand-copied

Standing constraint (card comment + ops doctrine): nucula hardware is defined in
`zeugmaster/nucula-board`; kernel board tables for nucula must be GENERATED from the
hardware repo, never hand-copied.

**What the hardware repo actually contains** (read via `gh api repos/zeugmaster/nucula-board`,
2026-10-21; default branch `main`, license **CERN-OHL-S-2.0**, last push 2026-10-09):

- Schematics: `nucula-v2.kicad_sch` (root) + sub-sheets `nfc.kicad_sch`,
  `power-mcu.kicad_sch`, `oled.kicad_sch`, `keyboard.kicad_sch`; board `nucula-v2.kicad_pcb`.
- **`docs/netlist.xml`** — an exported KiCad netlist (S-expression): every component,
  reference designator, pad, and net name. This is the machine-readable wiring.
- Precedent for normalized derivatives: `docs/oled24/pinout.json` and
  `manufacturing/production-2026-10-09/validation/display-mapping-check.json` — the repo
  already curates small JSON extracts next to the CAD sources.

**Generator input format (proposal).** The generator reads **`docs/netlist.xml`** from a
pinned commit of `zeugmaster/nucula-board` and extracts, by reference designator + net
name: the ESP32-C3 module's GPIO nets for `SDA/SCL/IRQ/VEN` of the PN7160 (U-ref on
`nfc.kicad_sch`), the OLED bus, USB D+/D−, and the keyboard matrix. Net-name matching is
via a **small sidecar map checked into ccid** (`crates/boards/gen/nucula-nets.toml`:
`"PN7160.SDA" → net "I2C_SDA" → esp_gpio 4` style entries with designator anchors), so a
renamed net upstream changes ONE sidecar line, not the parser. Output:
`crates/boards/src/nucula_gen.rs`, checked in, with a header the CI check (§5) verifies:

```rust
// @generated from zeugmaster/nucula-board@<rev> docs/netlist.xml
// netlist sha256: <…>   — regenerate: cargo run -p boards-gen -- --board nucula
// DO NOT EDIT: hand edits are overwritten and fail `boards check` (§5).
```

Cross-check on landing: the generated table MUST equal the historical bench-proven
constants (`crates/pn7160-nci/src/board.rs:30-36`: addr 0x28, sda 4, scl 5, irq 6, ven 7,
asserted by `board.rs:46-53` tests and mirrored in `pn7160_ccid.rs:125-131`). If the
generator disagrees with the bench, the bench wins and the disagreement becomes a finding
against the netlist revision — exactly the interop-over-spec doctrine applied to data.

**Fork-vs-consume — open decision O-1, with recommendation.**

| option | shape | when it wins |
|---|---|---|
| **Consume (recommended)** | read-only dependency on `zeugmaster/nucula-board` at generation time only; sidecar net-map + pinned rev live in ccid | upstream is alive (pushed 2026-10-09), CERN-OHL-S-2.0 permits use; no Amperstrand CAD maintenance burden; the EXTERNAL-POST GATE (ops doctrine, 2026-10-21) forbids automated interaction with non-org repos anyway, so a fork's PR-back workflow adds nothing today |
| Fork (`Amperstrand/nucula-board`) | we own a mirror; generator reads our default branch; upstream changes ride a manual merge | we start shipping schematic changes (the "nucula all-in-one" product milestone) or upstream goes dormant while rev-A/B units are still in service |

Recommendation: **consume now; fork at the first schematic change we need to carry.** The
generator + sidecar make either posture a one-line repoint, so the decision is reversible
and cheap to defer — record it in `docs/DECISION_REGISTER.md` when the implementation
card lands.

### 2.2 COTS boards (m5stick, m5atom) — a checked-in Grove map table, citing bolty B2

Commercial-off-the-shelf boards have no hardware repo of ours to generate from; their
source of truth is the **vendor pinout + our bench evidence**. The table IS the artifact:

- The Grove I2C facts come from bolty's lesson **B2** (`bolty-rs docs/lessons-learned.md`
  B2: m5stick SDA=GPIO32/SCL=GPIO33 with MFRC522 @ 0x28; m5atom SDA=26/SCL=32 — "the
  `board-m5atom` layout finds nothing on this rig") and are bench-confirmed in this repo
  (`main.rs:639-642`).
- Each COTS table row carries a `provenance` field naming the vendor doc + the bench
  lesson (`prov: "M5Stack StickC pinout; bolty B2; bench 2026-08-25"`), so the data is
  auditable without leaving the file.
- `firmware/esp32-ccid/Cargo.toml:24-26` currently **quotes bolty docs**; after migration
  that comment becomes a pointer: "pin data lives in `crates/boards` — do not add numbers
  here" (§4 step 1). Bolty's copy gets the same treatment in bolty's flip PR (§3.4).

### 2.3 F469 / F746 — wired-contact minimal tables

STM32 boards are contact-only today (card context: "stm32f469 = wired contact only";
`docs/MILESTONES.md` M0-M3 records the Cherry ST-2xxx contact path via pcscd). Their
wiring lives in type-state code — `firmware/ccid-firmware/src/board_config.rs`:
- f469 (`board_config.rs:12-111`): USART2 smartcard, IO=PA2 AF7 open-drain, CLK=PA4 AF7,
  RST=PG10, PRES=PC2, PWR=PC5, USB DM/DP=PA11/PA12 AF10;
- f746 (`board_config.rs:113-182`): bitbang, IO=PI0, CLK=PF6, RST=PI2, PRES=PF10,
  PWR=PF7, USB PA11/PA12.

That file's header already claims a local version of the one-file property ("Adding a new
board: add a new `#[cfg]` module here, add one line to main.rs" — `board_config.rs:7-8`).
The design keeps it as the Layer-B constructor and adds a **minimal Layer-A table**: pins
by name (`sc_io`, `sc_clk`, `sc_rst`, `sc_pres`, `sc_pwr`, `usb_dm`, `usb_dp`),
`wired_contact: true`, `nfc: None`, `OtaFamily::BareMetal` (no partition table — memory
comes from `memory.x` / `memory-f746.x`; no OTA slots, flash-bundle carries a full image
only). Minimal = the table records what presets/CI/docs need (capabilities, USB identity,
target) and does NOT try to express AF-mode type-state — that stays in the constructor
where the compiler can check it.

---

## 3. Consumption story

### 3.1 Frontend constructors (the firmware bins)

Today the bin hand-writes cfg blocks per board (`main.rs:639-646`), passes raw numbers to
the shared adapters (`I2cMasterBus::new(0, sda, scl)`, `main.rs:660`), and passes typed
tokens to the LCD (`main.rs:690-700`). After migration, the same bin contains per-board
constructor functions that READ the table:

```rust
// firmware/esp32-ccid/src/board_ctor.rs (pilot: m5stick)
use boards::{BusKind, BusRole};
pub fn frontend(peripherals: Peripherals) -> … {
    let b = boards::M5STICK;
    let nfc_bus = b.bus(BusRole::NfcFrontend).i2c();          // (0, 32, 33)
    let mut bus = pn7160_nci::hal_i2c::I2cMasterBus::new(
        nfc_bus.port, nfc_bus.sda, nfc_bus.scl).expect("I2C0 init");
    let dev = bus.add_hal_device(b.nfc.i2c_addr, nfc_bus.hz_run).expect("dev");
    …
}
```

Board *selection* stays a Cargo feature (`board-m5stick`) — features remain the no_std
compile-time selector; what changes is that a feature now selects a **table entry**
instead of carrying pin data inline. The three `compile_error!` guards in `main.rs:6-26`
collapse into one registry-driven guard (unknown board feature ⇒ the boards crate's
`ALL_BOARDS` lookup fails to resolve), and the pin-data-bearing error string
(`main.rs:26`) is generated from the table so it can never disagree with it.

### 3.2 Relationship to the ccid-service board-blind boundary (#383)

`crates/ccid-service/Cargo.toml` states the boundary: the serving kernel "may depend ONLY
on card-interface, ccid-core, ccid-protocol, ccid-transport-serial … It must NEVER see
board/frontend internals (pins, peripherals, **board tables**): the firmware bin owns
those and hands the service a constructed card driver." (header comment,
`crates/ccid-service/Cargo.toml`).

This design **strengthens** that boundary rather than touching it:

- `crates/boards` is consumed by the **bins** (`firmware/esp32-ccid`,
  `firmware/ccid-firmware`), by xtask/preset tooling, and by CI checks — never by
  `ccid-service`. The union-matrix `service-boundary` job
  (`.github/workflows/union-matrix.yml:221-246`) and `tools/union_matrix.py check`'s
  manifest allowlist keep enforcing exactly the current dep set; adding `boards` to
  ccid-service would fail that check, which is the desired tripwire.
- The dependency direction makes the blindness mechanical: data (boards) → constructor
  (bin) → trait object/driver (`NfcDriver`, `crates/ccid-service/src/nfc.rs` via the
  `RetryFrontend`/`EitherFrontend` wrappers, `crates/ccid-service/src/frontend.rs`) →
  service. A board fact can reach the service only by being compiled INTO a constructed
  driver — same as today, now with one place for the facts.

### 3.3 Product presets (M3 lane: `cargo xtask build product=X`)

The product model (epic `t_506dfe03`): named products — reader-min, provisioner-min,
connected, nucula all-in-one, stm32-f469 — composed over one kernel. The M3 presets lane
(no `xtask/` in-repo yet; `tools/` is the current tooling home) will render
`product × board` into concrete build args. The board tables are the contract that makes
presets safe to compose:

- A preset declares **wants** (`lcd`, `wifi`, `wired_contact`, NFC controller class);
  the board table declares **haves** (`caps`, `nfc.controller`). Preset resolution =
  wants ⊆ haves, checked by xtask BEFORE the build, e.g. `reader-min` on m5stick ⇒
  MFRC522 frontend + `partitions-ota.csv` family; `reader-min` on nucula ⇒ PN7160 +
  same family; `nucula-all-in-one` on m5stick ⇒ **refused** (no PMU/OLED capability),
  replacing today's implicit feature-coupling (e.g. `pn7160-bringup = […, "board-nucula"]`
  hard-wires that pairing in `Cargo.toml:35`).
- The resolved tuple drives: cargo features (selection), the board's
  `ota_partition_family` (partition CSV + bundle family, §6), console/wire facts (bench
  tool args — stopbits 2 vs 1), and the FWID variant string (today hand-passed at
  `main.rs:699`: `"GemPC serial"`).

### 3.4 bolty consumes (rev-pinned, the pn7160-nci precedent)

bolty already consumes this repo's board data: `bolty-rs crates/bolty-pn7160/Cargo.toml`
pins `pn7160-nci = { git = …ccid-firmware-rs…, rev = "0b8e33e3…" }` and
`bolty-pn7160/src/lib.rs:29` re-exports `pn7160_nci::board::{BoardPins, NUCULA}`;
bolty's nucula firmware uses gpio4/5/6/7 from it (`bolty-rs apps/bolty-esp32/src/
firmware_nucula.rs:209-212`). The boards crate follows the same lane:

1. ccid lands `crates/boards` (path dep internally; bolty unaffected).
2. bolty's flip PR (bolty repo, not this one): add `boards = { git = …, rev = <pin> }`,
   replace bolty's inline Grove maps (`apps/bolty-esp32/Cargo.toml:18-19` feature
   comments, `src/display.rs:193-195` pin args) with table reads, keep B2 as the
   provenance citation.
3. `docs/SHARED-CRATES.md` gains a `boards` row (home ccid, consumer bolty, pin) and the
   ops drift check watches it — the manifest's own scope note says internal crates join
   "the day bolty-rs starts consuming one via a git dep".

---

## 4. Migration path — smallest first, m5stick as pilot

Each step is an independently mergeable PR; the lane order respects the ACTIVE M2 lane by
touching no kernel crate except the new one.

| step | scope | files | acceptance |
|---|---|---|---|
| 0 | **Land `crates/boards`** — schema + m5stick + m5atom tables + host tests (per `pn7160-nci/src/board.rs:38-66` test pattern: assert every field against the cited source); no consumer changes | `crates/boards/**` only | host tests green in union-matrix `host-union-check` shape; zero behavior change anywhere |
| 1 | **Pilot: m5stick consumes** — `main.rs` cfg tuples → `boards::M5STICK` reads; LCD/LED call sites read the table (typed-token call behind a number→token match, asserted by a `const`-time check that the table numbers equal the tokens used); `Cargo.toml:24-26` comment becomes a pointer, pin data removed from the `compile_error!` string | `firmware/esp32-ccid/src/main.rs`, `m5stick_lcd.rs` (call-site only), `Cargo.toml` (comments) | build.sh `m5stick` + `m5stick-plus-lcd` variants byte-compatible behavior; **the acceptance test from the card: with step 0+1 done, adding `m5atom` needed ZERO new pin data anywhere** — it rode the same table |
| 2 | **m5atom** — same constructor path as m5stick (identical MFRC522 wiring, only bus pins differ — the table's whole point) | constructor file | `build.sh m5atom` green; one-file test holds |
| 3 | **nucula generated** — generator + sidecar net-map + checked-in `nucula_gen.rs`; host test asserts `boards::NUCULA`'s PN7160 block **equals** `pn7160-nci::board::NUCULA` (the crate's own tests, `board.rs:46-53`, stay untouched — no rev bump forced on bolty; a mismatch fails CI, making drift impossible instead of merely discouraged) | `crates/boards/**`, `tools/` generator | generated == bench constants; `cargo check -p boards` offline (no network at build) |
| 4 | **STM32 minimal tables** — F469/F746 Layer-A rows; `board_config.rs` constructors unchanged | `crates/boards/**`, one registry line in `firmware/ccid-firmware` (FWID/board name only) | `cargo check -p ccid-firmware` per existing STM32 CI legs |
| 5 | **bolty flip** (bolty repo PR) + `SHARED-CRATES.md` row + drift-check entry | bolty repo, ops script registry | bolty builds against the pin; B2 stays as citation, not as data |

**"Adding a board touches one file"** — precisely: the board's substantive content is one
new module `crates/boards/src/<board>.rs`. Rust still requires mechanical single-line
registrations the design cannot honestly elide: a `pub mod <board>;` line and (until M3
presets replace per-board features) a `board-<name> = []` feature line in the bin's
Cargo.toml. The design closes the gap with machine enforcement instead of denial:
`tools/union_matrix.py check` (§5) grows a boards-consistency leg — registry ↔ module
files ↔ bin features ↔ (for nucula) generator output — so a missing mechanical line fails
CI loudly rather than drifting silently. Acceptance test for the implementation card:
add a hypothetical board by touching ONLY the new module + the two one-liners, with the
CI check proving nothing else was edited (no pin data outside `crates/boards`, verified
by grep gate: `git grep -nE 'sda|scl|gpio[0-9]+' -- firmware/*/Cargo.toml` returns
comments-that-point only).

---

## 5. Union-matrix CI gains a boards axis (#390)

Today `tools/union_matrix.py` parses `firmware/esp32-ccid/build.sh` live ("the bench
authority"), validates features/sdkconfigs against the manifest (`check` mode), and
emits the matrix consumed by `.github/workflows/union-matrix.yml:84-106`; CI-only combos
sit in its `CI_ONLY_COMBOS` table with provenance. The boards crate becomes a **second,
equal manifest** the check cross-validates:

1. **`union_matrix.py check` grows boards assertions:** every `board-*` feature in
   `firmware/esp32-ccid/Cargo.toml` resolves to a `boards::ALL_BOARDS` entry with the
   right chip/target; every registry entry has a feature (or a preset that selects it);
   every board's `ota_partition_family` CSV exists (it already validates sdkconfigs —
   same pattern); nucula's generated file header matches the pinned netlist sha256.
2. **Boards axis in the matrix:** combos today are `backend × board × transport × mode`
   implicitly (feature strings). The board table makes the axis explicit — each esp
   combo leg gains `board.chip`, `board.ota_partition_family` as matrix dimensions, so a
   new board appears in the matrix the moment it appears in the registry (same
   "cannot drift from build.sh" property the workflow header promises, extended to the
   registry), and artifact names/step summaries can carry the family (§6).
3. **Host-testable board data rides `host-union-check`** (the x86_64 leg,
   `union-matrix.yml:192-219`): schema sanity (no pin double-claims within a board
   except documented conflicts like GPIO27 — encoded in `caps` exclusivity), Grove-map
   assertions vs the cited lessons, `NUCULA`-equality with pn7160-nci (§4 step 3).
   Seconds per leg, no esp toolchain — the cheapest CI surface in the repo.
4. **Opt-in policy unchanged:** `ci-run` label gate and push-leg path filters
   (`union-matrix.yml:14-19`, `:70-83`) keep applying; docs-only merges still skip the
   esp legs. The boards crate itself compiles + tests under the host legs (which DO run
   on docs-only merges), giving the design's data layer continuous coverage without
   opting every PR into 13 IDF compiles.

---

## 6. OTA-partition family ↔ flash-bundle (`amperstrand.flash-bundle/1`)

`docs/FLASH-VIA-LABGRID-DESIGN.md` defines the bundle manifest schema
`amperstrand.flash-bundle/1` (§ manifest, line ~348): every bundle records a `family`
(chip, esp-idf version, bootloader era) and a `layout` **parsed from the partition CSV
at build time** (`"source": "partitions-ota.csv"`, line ~362), with a refusal matrix for
app-only flashes on family/layout mismatch (lines ~438-442) and bootloader offset
**derived from chip family** — esp32 (xtensa) → 0x1000, C3/S3/C2 → 0x0 (§4.4 check 7,
lines ~375/428). The 2026-10-20 mixed-family boot-loop incident is why the schema
exists at all (design line ~20: "old prebuilt bootloader v5.5.1 + freshly-built v5.5.5
images + two partition-table families").

The board table's `ota_partition_family` column is the **board-side half of that
contract** — today the mapping board → partition CSV → bundle family lives implicitly in
build.sh variant blocks (`FLASH_OFFSET="0x40000"  # ota_0 slot per partitions-ota.csv`,
build.sh:219 etc.) and in the single CSV `firmware/esp32-ccid/partitions-ota.csv`
(nvs @0x9000, otadata @0x30000, ota_0 @0x40000 0x1A0000, ota_1 @0x1E0000, coredump
@0x3F0000). After migration:

- the table is the ONE place a board names its CSV + slot offsets + bootloader-offset
  rule (chip-derived);
- xtask/preset resolution (§3.3) emits the same values into the build args AND the
  bundle manifest, so the manifest's `family`/`layout` can never disagree with the
  board that produced the image (the refusal matrix then compares device-vs-bundle on
  data both sides derived from one table);
- STM32 rows carry `OtaFamily::BareMetal` (no OTA slots — `memory.x`/`memory-f746.x`
  link scripts; full-image bundles only), keeping the same one-column vocabulary across
  families so the flash tooling's refusal matrix has an explicit "no-slot" member rather
  than an absence.

---

## Open decisions (to be recorded in `docs/DECISION_REGISTER.md` by the implementation card)

- **O-1 fork-vs-consume for `zeugmaster/nucula-board`** — recommendation: consume now,
  fork at first carried schematic change (§2.1). External-post gate: any upstream PR
  needs owner signoff regardless.
- **O-2 crate name** — default `boards` (§1.4).
- **O-3 feature collapse** — whether M3 presets eventually replace per-board cargo
  features entirely (registry selected by preset name), or features remain the
  compile-time selector with presets as sugar. Design supports both; decide at M3.

## Evidence appendix — every current-state claim above

- Grove maps quoted from bolty: `firmware/esp32-ccid/Cargo.toml:24-26`; nucula pins in
  comments `:29-33`; feature graph `:17-59`; MFRC522 default `:18`.
- cfg pin tuples + guards: `firmware/esp32-ccid/src/main.rs:6-26`, `:639-646`;
  I2C bus + probe `:650-674`; LCD call site `:687-713`; LED-vs-LCD exclusivity
  `:682-716`.
- LCD pins + AXP192 I2C1: `firmware/esp32-ccid/src/m5stick_lcd.rs:405-421` (signature),
  `:413-421` param types; LED matrix GPIO27: `src/led.rs:4`, `:57`.
- PN7160 wiring: `firmware/esp32-ccid/src/pn7160_ccid.rs:125-131`;
  `crates/pn7160-nci/src/board.rs:13-36` (+ tests `:38-66`);
  re-export `crates/pn7160-nci/src/esp_transport.rs:13`.
- STM32 wiring: `firmware/ccid-firmware/src/board_config.rs:12-111` (f469),
  `:113-182` (f746), header `:7-8`; HAL/dep shape
  `firmware/ccid-firmware/Cargo.toml:56-66`.
- ccid-service boundary: `crates/ccid-service/Cargo.toml` header comment + dep list;
  boundary CI job `.github/workflows/union-matrix.yml:221-246`; matrix generation
  `:84-106`; opt-in/path-filter `:14-19`, `:70-83`; host leg `:192-219`.
- union-matrix manifest: `tools/union_matrix.py` module docstring + `CI_ONLY_COMBOS`.
- Partitions: `firmware/esp32-ccid/partitions-ota.csv`; offsets used per variant
  `firmware/esp32-ccid/build.sh:219` (and siblings); variants `:44-70`.
- flash-bundle schema: `docs/FLASH-VIA-LABGRID-DESIGN.md` (schema `amperstrand.flash-
  bundle/1` ~line 348; family ~356; layout-from-CSV ~362; bootloader-offset rule §4.4
  check 7 ~375/428; refusal matrix ~438-442; incident trigger ~20).
- Kernel graph + scope note (internal vs cross-repo crates): `docs/SHARED-CRATES.md`
  (scope note t_72e1e2ed; pn7160-nci row; bump rules).
- Vendor/patch manifest state: `docs/vendor-patches.md` (per-fork pin table; scope split
  vs SHARED-CRATES).
- bolty side: `bolty-rs docs/lessons-learned.md` B2; `apps/bolty-esp32/Cargo.toml:18-22`;
  `apps/bolty-esp32/src/firmware_nucula.rs:209-212`; `src/display.rs:193-195`;
  `crates/bolty-pn7160/Cargo.toml` (rev 0b8e33e3…) + `src/lib.rs:29`;
  `docs/unified-board-config.md` (runtime-detection alternative, rejected here).
- nucula hardware repo: `zeugmaster/nucula-board` (gh api, 2026-10-21): CERN-OHL-S-2.0;
  `nucula-v2.kicad_sch`/`nfc.kicad_sch`/`power-mcu.kicad_sch`/`oled.kicad_sch`/
  `keyboard.kicad_sch`/`nucula-v2.kicad_pcb`; `docs/netlist.xml`;
  `docs/oled24/pinout.json`; last push 2026-10-09.
