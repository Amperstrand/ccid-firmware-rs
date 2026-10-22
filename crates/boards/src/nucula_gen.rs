//! @generated from zeugmaster/nucula-board@78e5891bb76745feffe0c2ac38e27e92bc887c31 docs/netlist.xml (netlist sha256 520b07791f390b7d5eea90824531773e7555caf1fde4b1b927a5e4ab249eeb0f)
//! regenerate: cargo run -p boards-gen -- --board nucula
//! DO NOT EDIT: hand edits are overwritten and fail the boards-consistency
//! check (design §5). Sources beyond the netlist are cited inline; the
//! bench-proven cross-check lives in the tests below.

use crate::schema::*;

/// nucula — ESP32-C3 + PN7160 (zeugmaster hardware repo, rev-B-draft).
/// Every GPIO number below is extracted from the pinned netlist by the
/// sidecar-anchored generator; the bench constants they MUST equal live
/// in `crates/pn7160-nci/src/board.rs` (see the tests).
pub const NUCULA: BoardDesc = BoardDesc {
    // ---- identity ----
    name: "nucula",
    chip: Chip::Esp32c3, // → target riscv32imc-esp-espidf
    variant_note: Some(
        "rev-B-draft hardware repo @78e5891bb76745feffe0c2ac38e27e92bc887c31; R23/R24 strap the NCI address — nominal 0x28, \
         probe before assuming (strap margin confirmed on the bench, #62)",
    ),

    // ---- buses ----
    buses: &[Bus {
        role: BusRole::NfcFrontend,
        kind: BusKind::I2c(I2cBus {
            port: 0,
            sda: 4, // netlist net /I2C_SDA: U6 I2C_SDA ↔ U3 IO4
            scl: 5, // netlist net /I2C_SCL: U6 I2C_SCL ↔ U3 IO5
            // No separate probe speed: driver_ng probes at bus level.
            // Run speed is the shared-bus limit — the PCF8574T keyboard
            // expander caps I2C at 100 kHz (pn7160-nci board.rs rule).
            hz_probe: 0,
            hz_run: 100_000,
        }),
    }],

    // ---- named pin roles (netlist nets /NFC_IRQ / /NFC_VEN) ----
    pins: &[
        Pin { role: "nfc_irq", gpio: 6 }, // U6 IRQ ↔ U3 IO6
        Pin { role: "nfc_ven", gpio: 7 }, // U6 VEN ↔ U3 IO7
    ],

    // ---- NFC peripheral on the frontend bus ----
    nfc: Some(NfcPeripheral {
        controller: NfcController::Pn7160,
        // Strap-derived nominal (R23/R24) — bench-proven constant, NOT a
        // netlist fact; equality with pn7160-nci::board::NUCULA is
        // asserted below so drift fails CI.
        i2c_addr: 40,
    }),

    // ---- capabilities ----
    caps: Caps {
        lcd: Cap::None,        // SSD1309 OLED @0x3C exists on hw (sheet 4) — no
                               // firmware capability yet
        led: Cap::None,        // no status LED
        axp192_pmu: Cap::None, // no AXP192 (dedicated PMU rails instead)
        ble: Cap::Feature("ble"),            // build.sh c3-ccid-ble
        spp: Cap::None,                       // ESP32-C3 has no BR/EDR classic
        wifi_vpcd: Cap::Feature("wifi-vpcd"), // build.sh wifi-vpcd (nucula leg)
        wired_contact: false,                 // NFC only
    },

    // ---- console + wire ----
    console: Console {
        kind: ConsoleKind::UsbCdc, // ROM USB-Serial/JTAG on GPIO18/19
        baud: 115_200,
        stopbits: 1, // CDC 8N1 (esp32_flash.py — vs the GemPC 8N2 UART)
    },
    wire: Wire::GemPcSerial, // GemPC Twin emulation over the CDC console

    // ---- flash/OTA family (design §6) ----
    ota_partition_family: OtaFamily::EspIdfOta {
        csv: "firmware/esp32-ccid/partitions-ota.csv", // build.sh c3 FLASH_OFFSET
        app_slot_offset: 0x40000,                      // ota_0 slot per the CSV
        bootloader_offset: 0x0,                        // esp32c3/riscv32 rule (§6)
    },

    // ---- provenance ----
    provenance: "GENERATED from zeugmaster/nucula-board@78e5891bb76745feffe0c2ac38e27e92bc887c31 docs/netlist.xml (sha256 520b07791f390b7d…); bench cross-check crates/pn7160-nci/src/board.rs (addr 0x28, sda 4, scl 5, irq 6, ven 7, I2C ≤100 kHz PCF8574T); console esp32_flash.py CDC 8N1; OTA build.sh c3 + partitions-ota.csv",
};

/// `board-nucula` — the Cargo feature that selects this table entry.
pub const FEATURE: &str = "board-nucula";

#[cfg(test)]
mod tests {
    use super::*;

    /// The §2.1 landing cross-check: the GENERATED table equals the
    /// historical bench-proven constants — drift is impossible, not
    /// merely discouraged. (pn7160-nci's own board.rs tests stay
    /// untouched: no rev bump forced on bolty.)
    #[test]
    fn generated_pn7160_block_equals_pn7160_nci_board() {
        let bench = pn7160_nci::board::NUCULA;
        let bus = NUCULA.bus(BusRole::NfcFrontend).i2c();
        assert_eq!(bus.sda as i32, bench.sda, "sda: netlist vs bench");
        assert_eq!(bus.scl as i32, bench.scl, "scl: netlist vs bench");
        assert_eq!(
            NUCULA.pin("nfc_irq") as i32,
            bench.irq,
            "irq: netlist vs bench"
        );
        assert_eq!(
            NUCULA.pin("nfc_ven") as i32,
            bench.ven,
            "ven: netlist vs bench"
        );
        assert_eq!(
            NUCULA.nfc.expect("nucula: NFC frontend").i2c_addr,
            bench.addr,
            "i2c addr: strap sidecar vs bench"
        );
    }

    /// Per-field bench constants, spelled out (the pn7160-nci test
    /// pattern): if the netlist disagrees, the bench wins and this test
    /// names the finding against the netlist revision.
    #[test]
    fn nucula_matches_the_historical_bench_constants() {
        let bus = NUCULA.bus(BusRole::NfcFrontend).i2c();
        assert_eq!((bus.port, bus.sda, bus.scl), (0, 4, 5));
        assert_eq!(
            (NUCULA.pin("nfc_irq"), NUCULA.pin("nfc_ven")),
            (6, 7)
        );
        assert_eq!(NUCULA.nfc.unwrap().i2c_addr, 40);
        // Shared-bus rule: the PCF8574T caps the bus at 100 kHz.
        assert_eq!(bus.hz_run, 100_000);
    }

    #[test]
    fn identity_and_family() {
        assert_eq!(NUCULA.name, "nucula");
        assert_eq!(NUCULA.chip, Chip::Esp32c3);
        assert_eq!(NUCULA.chip.target(), Some("riscv32imc-esp-espidf"));
        assert_eq!(FEATURE, "board-nucula");
        assert!(crate::ALL_BOARDS.iter().any(|b| b.name == "nucula"));
        let OtaFamily::EspIdfOta {
            csv,
            app_slot_offset,
            bootloader_offset,
        } = NUCULA.ota_partition_family
        else {
            panic!("nucula is an esp-idf OTA board");
        };
        assert_eq!(csv, "firmware/esp32-ccid/partitions-ota.csv");
        assert_eq!((app_slot_offset, bootloader_offset), (0x40000, 0x0));
    }
}
