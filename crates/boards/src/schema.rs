//! The `BoardDesc` schema — the type definitions the per-board tables
//! instantiate (design §1.2). Every field is const data by intent:
//! `no_std`-friendly, visible in rustdoc, `PartialEq`-comparable so drift
//! assertions and generated-vs-bench equality checks (§4 step 3) are free.

/// MCU family — selects the target triple + bootloader-offset rule
/// (design §6: esp32/xtensa → 0x1000, esp32c3 → 0x0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    Esp32,
    Esp32c3,
    Stm32f469,
    Stm32f746,
}

impl Chip {
    /// The esp-idf/std target triple (None for bare-metal STM32 legs).
    pub const fn target(self) -> Option<&'static str> {
        match self {
            Chip::Esp32 => Some("xtensa-esp32-espidf"),
            Chip::Esp32c3 => Some("riscv32imc-esp-espidf"),
            Chip::Stm32f469 | Chip::Stm32f746 => None,
        }
    }
}

/// What a bus is for on this board (lookup key in `BoardDesc::bus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusRole {
    /// The NFC reader frontend (Grove I2C on M5Stack boards).
    NfcFrontend,
    /// The PMU/LCD auxiliary bus (AXP192 on I2C1 of the m5stick).
    PmuLcd,
}

/// A bus: controller number + raw GPIO numbers + speeds — the Layer-A
/// interface, mirroring how driver_ng is driven today (the #149 P3 shared
/// adapter claims the controller by port).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bus {
    pub role: BusRole,
    pub kind: BusKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusKind {
    I2c(I2cBus),
}

/// Raw-number I2C facts: `I2cMasterBus::new(port, sda, scl)` plus the
/// probe/run speeds the bring-up sequence uses (probe @100 kHz, run @400 kHz
/// on the bench-proven wiring).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct I2cBus {
    pub port: u8,
    pub sda: u8,
    pub scl: u8,
    /// 0 = no probe step before the run-speed device claim.
    pub hz_probe: u32,
    pub hz_run: u32,
}

impl Bus {
    /// The I2C facts; panics if this bus is not I2C — acceptable at
    /// bring-up (const tables, a wrong-kind lookup is a programming error).
    pub fn i2c(&self) -> &I2cBus {
        match &self.kind {
            BusKind::I2c(bus) => bus,
        }
    }
}

/// A named pin role → raw GPIO number; kills magic numbers at call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pin {
    pub role: &'static str,
    pub gpio: u8,
}

/// The NFC peripheral on the frontend bus — per-board, not per-driver
/// (which reader chip + at which I2C address).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NfcPeripheral {
    pub controller: NfcController,
    pub i2c_addr: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfcController {
    Mfrc522,
    Pn7160,
    Pn532,
}

/// Capability presence: absent = compile-time "no" for preset composition.
/// `Feature` names the Cargo feature that turns the capability on
/// (features remain the compile-time selector — design §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Feature(&'static str),
    /// Unconditionally present, no feature gate.
    On,
    /// Not available on this board.
    None,
}

impl Cap {
    pub const fn is_available(&self) -> bool {
        match self {
            Cap::Feature(_) | Cap::On => true,
            Cap::None => false,
        }
    }
}

/// The preset composition surface (design §3.3): preset wants ⊆ board
/// haves, checked before the build. Encodes conflicts (m5stick GPIO27 is
/// the LCD backlight, so `led: Cap::None` even though the Atom LED matrix
/// driver exists — the exclusivity is data, not tribal knowledge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub lcd: Cap,
    pub led: Cap,
    pub axp192_pmu: Cap,
    pub ble: Cap,
    pub spp: Cap,
    pub wifi_vpcd: Cap,
    /// Wired contact pads (STM32 Cherry ST-2xxx path); NFC-only boards say false.
    pub wired_contact: bool,
}

/// Host-side console contract facts — the flash/bench tools already branch
/// on these (`esp32_flash.py --stopbits`: GemPC 8N2 vs CDC 8N1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Console {
    pub kind: ConsoleKind,
    pub baud: u32,
    pub stopbits: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleKind {
    /// UART0, the GemPC Twin emulation wire.
    Uart0GemPc,
    /// ROM USB-Serial/JTAG CDC (nucula: GPIO18/19, no UART bridge).
    UsbCdc,
    /// defmt/RTT over a debug probe (STM32 bare-metal legs — no serial
    /// console contract; `baud`/`stopbits` are unused placeholders).
    DebugProbe,
}

/// The wire protocol the firmware serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// GemPC Twin serial emulation over the console.
    GemPcSerial,
    /// Raw CCID over USB (STM32 usb_fs legs — the pcscd contact path;
    /// design §2.3 minimal tables).
    UsbCcid,
}

/// OTA-partition family — the board-side half of the
/// `amperstrand.flash-bundle/1` contract (design §6): ONE place names the
/// partition CSV, the app-slot offset, and the chip-derived bootloader
/// offset, so the bundle manifest can never disagree with the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaFamily {
    EspIdfOta {
        /// Path of the partition CSV, repo-root-relative.
        csv: &'static str,
        /// `ota_0` slot offset (build.sh FLASH_OFFSET).
        app_slot_offset: u32,
        /// Chip-derived (esp32/xtensa → 0x1000, C3 → 0x0).
        bootloader_offset: u32,
    },
    /// STM32: no OTA slots — `memory.x` link scripts, full-image bundles.
    BareMetal,
}

/// One board's complete wiring as const data — the schema the per-board
/// modules (`src/<board>.rs`) instantiate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardDesc {
    /// Identity in logs/CI/presets; FWID stamps.
    pub name: &'static str,
    pub chip: Chip,
    /// Per-model caveat a preset author must know (e.g. the m5stick
    /// ST7789-vs-ST7735S geometry split, issue #275).
    pub variant_note: Option<&'static str>,
    pub buses: &'static [Bus],
    pub pins: &'static [Pin],
    /// `None` on wired-contact-only boards (STM32).
    pub nfc: Option<NfcPeripheral>,
    pub caps: Caps,
    pub console: Console,
    pub wire: Wire,
    pub ota_partition_family: OtaFamily,
    /// Where this table's facts came from — vendor doc + bench lesson
    /// (design §2.2), so the data is auditable without leaving the file.
    pub provenance: &'static str,
}

impl BoardDesc {
    /// Look up a bus by role; panics if absent (const tables, bring-up —
    /// a missing role is a table/constructor programming error).
    /// Non-const by intent: str/enum `==` is not const-callable, and
    /// Layer-B constructors consume this at runtime anyway.
    pub fn bus(&self, role: BusRole) -> &Bus {
        self.buses
            .iter()
            .find(|b| b.role == role)
            .unwrap_or_else(|| panic!("board {}: no bus for role {:?}", self.name, role))
    }

    /// Look up a pin by role name; panics if absent.
    pub fn pin(&self, role: &str) -> u8 {
        self.pins
            .iter()
            .find(|p| p.role == role)
            .unwrap_or_else(|| panic!("board {}: no pin for role {}", self.name, role))
            .gpio
    }

    /// Const-callable [`pin`](Self::pin) — a byte-wise role compare,
    /// because `str ==` is not const-callable. Lets firmware assert a
    /// typed-token call site against the table at COMPILE time (design
    /// §4 step 1: a drifted pin fails the build, not the bench).
    pub const fn pin_const(&self, role: &str) -> u8 {
        let mut i = 0;
        while i < self.pins.len() {
            if role_bytes_eq(self.pins[i].role, role) {
                return self.pins[i].gpio;
            }
            i += 1;
        }
        panic!("board table: no pin for role (const lookup)")
    }

    /// Const-callable I2C pin facts `(sda, scl)` of the bus serving
    /// `role` — the bus-side twin of [`pin_const`](Self::pin_const):
    /// pins a Layer-B constructor's number→token match arm to the
    /// table at COMPILE time (same §4 step 1 rule: a drifted pin
    /// fails the build, not the bench).
    pub const fn bus_i2c_pins_const(&self, role: BusRole) -> (u8, u8) {
        let mut i = 0;
        while i < self.buses.len() {
            if bus_role_eq(self.buses[i].role, role) {
                // `BusKind` has a single I2c variant today, so this is
                // a one-arm match (clippy rejects an irrefutable
                // `if let`); a second bus kind turns the arm refutable
                // and the build fails HERE — the loud failure this
                // schema wants.
                match self.buses[i].kind {
                    BusKind::I2c(bus) => return (bus.sda, bus.scl),
                }
            }
            i += 1;
        }
        panic!("board table: no bus for role (const lookup)")
    }
}

/// Const-callable BusRole equality (derived `PartialEq` is not const;
/// a byte-wise-style compare on the discriminant, same trick as
/// [`role_bytes_eq`]).
const fn bus_role_eq(a: BusRole, b: BusRole) -> bool {
    matches!(a, BusRole::NfcFrontend) == matches!(b, BusRole::NfcFrontend)
}

const fn role_bytes_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{M5STICK, NUCULA};

    /// The const bus accessor agrees with the runtime `bus().i2c()`
    /// lookup on real tables — the compile-time pinning source for
    /// Layer-B number→token match arms (boards-as-data §4 step 1).
    #[test]
    fn bus_i2c_pins_const_agrees_with_the_runtime_lookup() {
        assert_eq!(NUCULA.bus_i2c_pins_const(BusRole::NfcFrontend), {
            let b = NUCULA.bus(BusRole::NfcFrontend).i2c();
            (b.sda, b.scl)
        });
        assert_eq!(NUCULA.bus_i2c_pins_const(BusRole::NfcFrontend), (4, 5));
        assert_eq!(M5STICK.bus_i2c_pins_const(BusRole::NfcFrontend), (32, 33));
    }
}
