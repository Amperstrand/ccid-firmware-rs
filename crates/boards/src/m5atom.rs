//! M5Stack Atom (Matrix/Lite) — COTS table, design §2.2: hand-written,
//! provenance cited (the table IS the artifact). Identical MFRC522
//! frontend wiring to the m5stick — only the Grove bus pins differ,
//! which is the boards-as-data design's whole point (§4 step 2: the
//! m5atom slice rode the same constructor + schema with zero new pin
//! data anywhere outside this file).

use crate::schema::*;

/// M5Stack Atom (bolty B2: "m5atom SDA=26/SCL=32 … the `board-m5atom`
/// layout finds nothing on this rig"). Same MFRC522 @ 0x28 frontend,
/// same partitions-ota.csv family, same GemPC UART0 wire as the m5stick.
pub const M5ATOM: BoardDesc = BoardDesc {
    // ---- identity ----
    name: "m5atom",
    chip: Chip::Esp32, // → target xtensa-esp32-espidf
    variant_note: Some(
        "bolty B2 rig caveat: with the MFRC522 wired to the vendor Grove \
         layout (26/32) the ai-legion rig probe found NOTHING — the table \
         records the vendor facts; an on-target probe decides at bring-up",
    ),

    // ---- buses: controller number + raw GPIO numbers (Layer-A interface) ----
    buses: &[Bus {
        role: BusRole::NfcFrontend,
        kind: BusKind::I2c(I2cBus {
            port: 0,
            sda: 26, // main.rs board-m5atom cfg tuple; bolty B2
            scl: 32, // main.rs board-m5atom cfg tuple; bolty B2
            hz_probe: 100_000,
            hz_run: 400_000, // shared frontend literals (as m5stick)
        }),
    }],

    // ---- named pin roles ----
    pins: &[
        // The Atom Matrix onboard 25-LED WS2812 diagnostic display
        // (led.rs: "GPIO27 via the ESP32 RMT peripheral"). This pin has
        // no dual claim on this board (contrast m5stick GPIO27 = the
        // LCD backlight), so caps.led is On.
        Pin { role: "led_matrix", gpio: 27 },
    ],

    // ---- NFC peripheral on the frontend bus ----
    nfc: Some(NfcPeripheral {
        controller: NfcController::Mfrc522,
        i2c_addr: 0x28, // bolty B2; the shared bring-up probe address
    }),

    // ---- capabilities (what presets may compose; absence = compile-time "no") ----
    caps: Caps {
        lcd: Cap::None,        // no LCD variant exists (build.sh: m5atom is
                               // "as m5stick, default features" — no lcd leg)
        led: Cap::On,          // onboard matrix, no feature gate (led.rs)
        axp192_pmu: Cap::None, // no AXP192 on the Atom
        ble: Cap::None,        // no m5atom ble variant (ESP32 BR/EDR stays
                               // an m5stick capability)
        spp: Cap::None,        // no m5atom spp variant
        wifi_vpcd: Cap::Feature("wifi-vpcd"), // CI_ONLY combo
                               // host-vpcd-mfrc522-m5atom
        wired_contact: false,  // no contact pads — NFC only
    },

    // ---- console + wire ----
    console: Console {
        kind: ConsoleKind::Uart0GemPc,
        baud: 115_200,
        stopbits: 2, // GemPC Twin contract: 8N2 (esp32_flash.py --stopbits 2)
    },
    wire: Wire::GemPcSerial, // GemPC Twin emulation, UART0

    // ---- flash/OTA family (design §6) ----
    ota_partition_family: OtaFamily::EspIdfOta {
        csv: "firmware/esp32-ccid/partitions-ota.csv", // build.sh m5atom FLASH_OFFSET
        app_slot_offset: 0x40000,                      // ota_0 slot per the CSV
        bootloader_offset: 0x1000,                     // esp32/xtensa rule (§6)
    },

    // ---- provenance (design §2.2: auditable without leaving the file) ----
    provenance: "M5Stack Atom vendor pinout; bolty-rs docs/lessons-learned.md B2 (incl. the finds-nothing rig caveat); bench wiring in this repo (main.rs board-m5atom tuple, led.rs GPIO27); evidence appendix docs/BOARDS-AS-DATA-DESIGN.md",
};

/// `board-m5atom` — the Cargo feature that selects this table entry
/// (features remain the compile-time selector, D-12). Note this is the
/// crate's `default` board feature.
pub const FEATURE: &str = "board-m5atom";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ALL_BOARDS;

    // Per-field assertions against the cited sources — the
    // pn7160-nci/src/board.rs test pattern: if a number here disagrees
    // with the bench, the bench wins and this test names the finding.

    #[test]
    fn identity() {
        assert_eq!(M5ATOM.name, "m5atom");
        assert_eq!(M5ATOM.chip, Chip::Esp32);
        assert_eq!(M5ATOM.chip.target(), Some("xtensa-esp32-espidf"));
        // B2's finds-nothing rig caveat must stay visible (§2.2).
        assert!(M5ATOM.variant_note.is_some());
    }

    #[test]
    fn grove_frontend_bus() {
        // main.rs board-m5atom cfg tuple + bolty B2: SDA=26 / SCL=32 —
        // the ONE wiring difference vs the m5stick.
        let bus = M5ATOM.bus(BusRole::NfcFrontend).i2c();
        assert_eq!((bus.port, bus.sda, bus.scl), (0, 26, 32));
        // Same bring-up speeds as the shared frontend code path.
        assert_eq!(bus.hz_probe, 100_000);
        assert_eq!(bus.hz_run, 400_000);
    }

    #[test]
    fn no_pmu_lcd_bus() {
        // The Atom has no AXP192/LCD auxiliary bus (contrast m5stick I2C1).
        assert!(M5ATOM.buses.iter().all(|b| b.role != BusRole::PmuLcd));
    }

    #[test]
    fn led_matrix_pin() {
        // led.rs: the Atom Matrix 5x5 WS2812 rides GPIO27 via RMT.
        assert_eq!(M5ATOM.pin("led_matrix"), 27);
    }

    #[test]
    fn no_pin_double_claims() {
        // Schema sanity (design §5.3): within a board, no pin is claimed
        // twice. The Atom has no documented dual-claim conflict.
        for (i, p) in M5ATOM.pins.iter().enumerate() {
            for q in M5ATOM.pins.iter().skip(i + 1) {
                assert!(p.gpio != q.gpio, "pin double-claim: {:?} vs {:?}", p, q);
            }
        }
    }

    #[test]
    fn nfc_peripheral() {
        let nfc = M5ATOM.nfc.expect("m5atom is an NFC board");
        assert_eq!(nfc.controller, NfcController::Mfrc522);
        assert_eq!(nfc.i2c_addr, 0x28); // bolty B2; the probe address
    }

    #[test]
    fn capabilities() {
        // The Atom: no LCD/PMU/BT capability, unconditional LED matrix,
        // wifi-vpcd composed in the CI_ONLY host leg.
        assert_eq!(M5ATOM.caps.lcd, Cap::None);
        assert_eq!(M5ATOM.caps.led, Cap::On);
        assert!(M5ATOM.caps.led.is_available());
        assert_eq!(M5ATOM.caps.axp192_pmu, Cap::None);
        assert_eq!(M5ATOM.caps.ble, Cap::None);
        assert_eq!(M5ATOM.caps.spp, Cap::None);
        assert_eq!(M5ATOM.caps.wifi_vpcd, Cap::Feature("wifi-vpcd"));
        assert!(!M5ATOM.caps.wired_contact); // NFC only, no contact pads
    }

    #[test]
    fn console_and_wire() {
        // "as m5stick, default features" (build.sh): the same GemPC Twin
        // UART0 contract — 115200 8N2.
        assert_eq!(M5ATOM.console.kind, ConsoleKind::Uart0GemPc);
        assert_eq!(M5ATOM.console.baud, 115_200);
        assert_eq!(M5ATOM.console.stopbits, 2);
        assert_eq!(M5ATOM.wire, Wire::GemPcSerial);
    }

    #[test]
    fn ota_family() {
        // build.sh m5atom: FLASH_OFFSET=0x40000 (ota_0 per
        // partitions-ota.csv); esp32/xtensa bootloader @ 0x1000.
        match M5ATOM.ota_partition_family {
            OtaFamily::EspIdfOta {
                csv,
                app_slot_offset,
                bootloader_offset,
            } => {
                assert_eq!(csv, "firmware/esp32-ccid/partitions-ota.csv");
                assert_eq!(app_slot_offset, 0x40000);
                assert_eq!(bootloader_offset, 0x1000);
            }
            other => panic!("expected EspIdfOta, got {:?}", other),
        }
    }

    #[test]
    fn registered() {
        // ALL_BOARDS carries every module (the §5 cross-check target).
        assert!(ALL_BOARDS.iter().any(|b| b.name == "m5atom"));
        // The selecting feature line matches the bin's feature name.
        assert_eq!(FEATURE, "board-m5atom");
    }
}
