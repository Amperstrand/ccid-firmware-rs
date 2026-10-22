//! M5StickC / StickC-Plus — the ai-legion bench rig board (COTS table,
//! design §2.2: hand-written, provenance cited — the table IS the artifact).
//!
//! Every number below is bench-proven wiring, not invented — see the
//! per-field `provenance` citations in the test module and the design's
//! evidence appendix (§1.2 "worked example" lists each source line).

use crate::schema::*;

/// M5StickC / StickC-Plus (bolty B2: "Grove I2C on SDA=GPIO32 / SCL=GPIO33
/// … MFRC522 at address 0x28").
pub const M5STICK: BoardDesc = BoardDesc {
    // ---- identity ----
    name: "m5stick",
    chip: Chip::Esp32, // → target xtensa-esp32-espidf
    variant_note: Some(
        "StickC-Plus LCD is a per-model capability (feature `lcd`): the pinned \
         ST7789 135x240 geometry mis-drives an original StickC's ST7735S \
         (Cargo.toml lcd feature, issue #275)",
    ),

    // ---- buses: controller number + raw GPIO numbers (Layer-A interface) ----
    buses: &[
        Bus {
            role: BusRole::NfcFrontend,
            kind: BusKind::I2c(I2cBus {
                port: 0,
                sda: 32,
                scl: 33, // main.rs board-m5stick cfg tuple; bolty B2
                hz_probe: 100_000,
                hz_run: 400_000, // probe@100k then run@400k (bring-up sequence)
            }),
        },
        Bus {
            role: BusRole::PmuLcd,
            kind: BusKind::I2c(I2cBus {
                port: 1,
                sda: 21,
                scl: 22, // AXP192 on I2C1 (m5stick_lcd.rs init)
                hz_probe: 0,
                hz_run: 400_000,
            }),
        },
    ],

    // ---- named pin roles (LCD block; m5stick_lcd.rs init signature) ----
    pins: &[
        Pin { role: "lcd_sclk", gpio: 13 },
        Pin { role: "lcd_mosi", gpio: 15 },
        Pin { role: "lcd_cs", gpio: 5 },
        Pin { role: "lcd_dc", gpio: 23 },
        Pin { role: "lcd_rst", gpio: 18 },
        // GPIO27 is the backlight — the Atom LED-matrix pin; exclusivity
        // with `led` is encoded in `caps` below.
        Pin { role: "lcd_bl", gpio: 27 },
    ],

    // ---- NFC peripheral on the frontend bus ----
    nfc: Some(NfcPeripheral {
        controller: NfcController::Mfrc522,
        i2c_addr: 0x28, // bolty B2; the bring-up probe site
    }),

    // ---- capabilities (what presets may compose; absence = compile-time "no") ----
    caps: Caps {
        lcd: Cap::Feature("lcd"),     // +lcd variant (build.sh m5stick-plus-lcd)
        led: Cap::None,               // GPIO27 is the backlight here; the Atom
                                      // LED matrix on 27 conflicts — encoded
                                      // as exclusivity, not tribal knowledge
        axp192_pmu: Cap::Feature("lcd"), // rides I2C1, claimed by the LCD init
        ble: Cap::Feature("ble"),     // build.sh m5stick-ble
        spp: Cap::Feature("spp"),     // build.sh m5stick-spp (ESP32 BR/EDR only)
        wifi_vpcd: Cap::Feature("wifi-vpcd"), // build.sh m5stick-vpcd
        wired_contact: false,         // no contact pads — NFC only
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
        csv: "firmware/esp32-ccid/partitions-ota.csv", // the ONLY such CSV today
        app_slot_offset: 0x40000,                      // build.sh FLASH_OFFSET
        bootloader_offset: 0x1000,                     // esp32/xtensa rule (§6)
    },

    // ---- provenance (design §2.2: auditable without leaving the file) ----
    provenance: "M5Stack StickC/StickC-Plus vendor pinout; bolty-rs docs/lessons-learned.md B2; bench-confirmed in this repo (main.rs board-m5stick wiring); evidence appendix docs/BOARDS-AS-DATA-DESIGN.md",
};

/// `board-m5stick` — the Cargo feature that selects this table entry
/// (features remain the compile-time selector, D-12).
pub const FEATURE: &str = "board-m5stick";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ALL_BOARDS;

    // Per-field assertions against the cited sources — the
    // pn7160-nci/src/board.rs test pattern: if a number here disagrees
    // with the bench, the bench wins and this test names the finding.

    #[test]
    fn identity() {
        assert_eq!(M5STICK.name, "m5stick");
        assert_eq!(M5STICK.chip, Chip::Esp32);
        assert_eq!(M5STICK.chip.target(), Some("xtensa-esp32-espidf"));
        assert!(M5STICK.variant_note.is_some()); // #275 ST7789/ST7735S split
    }

    #[test]
    fn grove_frontend_bus() {
        // main.rs board-m5stick cfg tuple + bolty B2: SDA=32 / SCL=33.
        let bus = M5STICK.bus(BusRole::NfcFrontend).i2c();
        assert_eq!((bus.port, bus.sda, bus.scl), (0, 32, 33));
        // Bring-up sequence: probe @100 kHz, then run @400 kHz.
        assert_eq!(bus.hz_probe, 100_000);
        assert_eq!(bus.hz_run, 400_000);
    }

    #[test]
    fn pmu_lcd_bus() {
        // AXP192 rides I2C1 on 21/22 (m5stick_lcd.rs init).
        let bus = M5STICK.bus(BusRole::PmuLcd).i2c();
        assert_eq!((bus.port, bus.sda, bus.scl), (1, 21, 22));
        assert_eq!(bus.hz_run, 400_000);
    }

    #[test]
    fn lcd_pin_block() {
        // m5stick_lcd.rs init signature order: sclk mosi cs dc rst bl.
        assert_eq!(M5STICK.pin("lcd_sclk"), 13);
        assert_eq!(M5STICK.pin("lcd_mosi"), 15);
        assert_eq!(M5STICK.pin("lcd_cs"), 5);
        assert_eq!(M5STICK.pin("lcd_dc"), 23);
        assert_eq!(M5STICK.pin("lcd_rst"), 18);
        assert_eq!(M5STICK.pin("lcd_bl"), 27);
    }

    #[test]
    fn no_pin_double_claims_except_documented_gpio27() {
        // Schema sanity (design §5.3): within a board, no pin is claimed
        // twice — except documented conflicts. GPIO27 lcd_bl is the ONE
        // such conflict here (backlight vs Atom LED matrix), and it is
        // documented by caps.led == Cap::None, so it must NOT appear twice.
        for (i, p) in M5STICK.pins.iter().enumerate() {
            for q in M5STICK.pins.iter().skip(i + 1) {
                assert!(p.gpio != q.gpio, "pin double-claim: {:?} vs {:?}", p, q);
            }
        }
        assert_eq!(M5STICK.caps.led, Cap::None); // the GPIO27 exclusivity
        assert!(!M5STICK.caps.led.is_available());
    }

    #[test]
    fn nfc_peripheral() {
        let nfc = M5STICK.nfc.expect("m5stick is an NFC board");
        assert_eq!(nfc.controller, NfcController::Mfrc522);
        assert_eq!(nfc.i2c_addr, 0x28); // bolty B2; the probe address
    }

    #[test]
    fn capabilities() {
        assert_eq!(M5STICK.caps.lcd, Cap::Feature("lcd"));
        assert_eq!(M5STICK.caps.axp192_pmu, Cap::Feature("lcd"));
        assert_eq!(M5STICK.caps.ble, Cap::Feature("ble"));
        assert_eq!(M5STICK.caps.spp, Cap::Feature("spp"));
        assert_eq!(M5STICK.caps.wifi_vpcd, Cap::Feature("wifi-vpcd"));
        assert!(!M5STICK.caps.wired_contact); // NFC only, no contact pads
    }

    #[test]
    fn console_and_wire() {
        // GemPC Twin contract: UART0 @115200 8N2 (esp32_flash.py --stopbits 2).
        assert_eq!(M5STICK.console.kind, ConsoleKind::Uart0GemPc);
        assert_eq!(M5STICK.console.baud, 115_200);
        assert_eq!(M5STICK.console.stopbits, 2);
        assert_eq!(M5STICK.wire, Wire::GemPcSerial);
    }

    #[test]
    fn ota_family() {
        // partitions-ota.csv: ota_0 @ 0x40000 (build.sh FLASH_OFFSET);
        // esp32/xtensa bootloader @ 0x1000 (flash-bundle §4.4 check 7).
        match M5STICK.ota_partition_family {
            OtaFamily::EspIdfOta { csv, app_slot_offset, bootloader_offset } => {
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
        assert!(ALL_BOARDS.iter().any(|b| b.name == "m5stick"));
        // The selecting feature line matches the bin's feature name.
        assert_eq!(FEATURE, "board-m5stick");
    }
}
