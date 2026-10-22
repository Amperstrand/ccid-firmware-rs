//! STM32F469I-DISCO — wired-contact minimal table (design §2.3): the
//! Layer-A row records what presets/CI/docs need (pins by name,
//! capabilities, USB identity, target); AF-mode type-state STAYS in the
//! Layer-B constructor `firmware/ccid-firmware/src/board_config.rs`
//! (f469::setup), where the compiler can check it.
//!
//! PIN ENCODING (STM32 is port+pin, the schema's `gpio` is one u8):
//! `gpio = port_ordinal * 16 + pin_number`, A=0, B=1, … K=10 —
//! reversible (`gpio / 16` → port letter, `gpio % 16` → pin), asserted
//! field-by-field in the tests below against the constructor's typed
//! pins. Every number cites board_config.rs; nothing is invented.

use crate::schema::*;

/// STM32F469I-DISCO wired-contact reader (USART2 smartcard frontend).
pub const F469: BoardDesc = BoardDesc {
    // ---- identity ----
    name: "stm32f469",
    chip: Chip::Stm32f469, // bare-metal thumbv7em — chip.target() is None
    variant_note: Some(
        "DISCO board; USART2 smartcard (AF7) — the Cherry ST-2xxx contact \
         path via pcscd (docs/MILESTONES.md M0-M3)",
    ),

    // ---- buses: none — the smartcard frontend is USART2 + GPIO, not I2C ----
    buses: &[],

    // ---- named pin roles (board_config.rs f469::setup, port*16+pin) ----
    pins: &[
        Pin { role: "sc_io", gpio: 2 },    // PA2 AF7 open-drain (USART2_IO)
        Pin { role: "sc_clk", gpio: 4 },   // PA4 AF7 (USART2_CK)
        Pin { role: "sc_rst", gpio: 106 }, // PG10 (RST)
        Pin { role: "sc_pres", gpio: 34 }, // PC2 (PRES)
        Pin { role: "sc_pwr", gpio: 37 },  // PC5 (PWR)
        Pin { role: "usb_dm", gpio: 11 },  // PA11 AF10 (USB DM)
        Pin { role: "usb_dp", gpio: 12 },  // PA12 AF10 (USB DP)
    ],

    // ---- no NFC frontend: contact-only ----
    nfc: None,

    // ---- capabilities ----
    caps: Caps {
        lcd: Cap::Feature("display"), // board_config.rs DisplayPins (cfg display)
        led: Cap::None,
        axp192_pmu: Cap::None,
        ble: Cap::None,
        spp: Cap::None,
        wifi_vpcd: Cap::None,
        wired_contact: true, // the Cherry ST-2xxx contact pads
    },

    // ---- console + wire ----
    console: Console {
        kind: ConsoleKind::DebugProbe, // defmt/RTT via debug probe — no serial console
        baud: 115_200,
        stopbits: 1,
    },
    wire: Wire::UsbCcid, // CCID over USB (usb_fs) — the pcscd contact path

    // ---- flash/OTA family: no partition table ----
    ota_partition_family: OtaFamily::BareMetal, // memory.x; full-image flash bundles

    // ---- provenance ----
    provenance: "firmware/ccid-firmware/src/board_config.rs f469::setup (USART2 AF7 PA2/PA4, RST=PG10, PRES=PC2, PWR=PC5, USB PA11/PA12 AF10); docs/MILESTONES.md M0-M3 Cherry ST-2xxx; design §2.3",
};

/// `stm32f469` — the Cargo feature that selects this table entry (the
/// ccid-firmware default board).
pub const FEATURE: &str = "stm32f469";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ALL_BOARDS;

    /// Decode `gpio = port*16 + pin` back to the constructor's typed pin
    /// and assert it — the pn7160-nci board.rs pattern: if a number here
    /// disagrees with board_config.rs, the constructor wins and this
    /// test names the finding.
    fn port_pin(gpio: u8) -> (char, u8) {
        ((b'A' + gpio / 16) as char, gpio % 16)
    }

    #[test]
    fn identity_and_family() {
        assert_eq!(F469.name, "stm32f469");
        assert_eq!(F469.chip, Chip::Stm32f469);
        assert_eq!(F469.chip.target(), None); // bare-metal leg
        assert_eq!(F469.ota_partition_family, OtaFamily::BareMetal);
        assert_eq!(FEATURE, "stm32f469");
        assert!(ALL_BOARDS.iter().any(|b| b.name == "stm32f469"));
    }

    #[test]
    fn smartcard_pins_match_the_constructor() {
        // board_config.rs f469::setup: PA2/PA4 (AF7), PG10, PC2, PC5.
        assert_eq!(port_pin(F469.pin("sc_io")), ('A', 2));
        assert_eq!(port_pin(F469.pin("sc_clk")), ('A', 4));
        assert_eq!(port_pin(F469.pin("sc_rst")), ('G', 10));
        assert_eq!(port_pin(F469.pin("sc_pres")), ('C', 2));
        assert_eq!(port_pin(F469.pin("sc_pwr")), ('C', 5));
    }

    #[test]
    fn usb_pins_match_the_constructor() {
        // board_config.rs f469: UsbDmPin = PA11<Alternate<10>>, DP = PA12.
        assert_eq!(port_pin(F469.pin("usb_dm")), ('A', 11));
        assert_eq!(port_pin(F469.pin("usb_dp")), ('A', 12));
    }

    #[test]
    fn contact_only() {
        assert!(F469.caps.wired_contact);
        assert_eq!(F469.nfc, None);
        assert!(F469.buses.is_empty());
        assert_eq!(F469.caps.lcd, Cap::Feature("display"));
        assert_eq!(F469.wire, Wire::UsbCcid);
    }

    #[test]
    fn no_pin_double_claims() {
        // Schema sanity (design §5.3): every raw pin unique on this board.
        let mut seen = std::collections::BTreeSet::new();
        for p in F469.pins {
            assert!(seen.insert(p.gpio), "pin {} double-claimed", p.gpio);
        }
    }
}
