//! STM32F746 — wired-contact minimal table (design §2.3): pins by name,
//! the bitbang smartcard frontend's wiring as data; AF/type-state stays
//! in `firmware/ccid-firmware/src/board_config.rs` (f746::setup).
//!
//! PIN ENCODING: `gpio = port_ordinal * 16 + pin_number`, A=0 … K=10 —
//! see stm32f469.rs; decoded and asserted in the tests below.

use crate::schema::*;

/// STM32F746 (DISCO) wired-contact reader (bitbang smartcard frontend).
pub const F746: BoardDesc = BoardDesc {
    // ---- identity ----
    name: "stm32f746",
    chip: Chip::Stm32f746, // bare-metal thumbv7em — chip.target() is None
    variant_note: Some(
        "bitbang smartcard frontend; setup drives the DISCO LCD backlight \
         (PK3) unconditionally but no display driver exists — lcd stays \
         Cap::None until one lands",
    ),

    // ---- buses: none — bitbang GPIO + USART-less frontend ----
    buses: &[],

    // ---- named pin roles (board_config.rs f746::setup, port*16+pin) ----
    pins: &[
        Pin { role: "sc_io", gpio: 128 },  // PI0 open-drain (bitbang IO)
        Pin { role: "sc_clk", gpio: 86 },  // PF6 push-pull (bitbang CLK)
        Pin { role: "sc_rst", gpio: 130 }, // PI2 push-pull (RST)
        Pin { role: "sc_pres", gpio: 90 }, // PF10 floating input (PRES)
        Pin { role: "sc_pwr", gpio: 87 },  // PF7 push-pull (PWR)
        Pin { role: "usb_dm", gpio: 11 },  // PA11 AF10 (USB DM)
        Pin { role: "usb_dp", gpio: 12 },  // PA12 AF10 (USB DP)
        Pin { role: "lcd_bl", gpio: 163 }, // PK3 — backlight driven in setup
    ],

    // ---- no NFC frontend: contact-only ----
    nfc: None,

    // ---- capabilities ----
    caps: Caps {
        lcd: Cap::None, // backlight pin only — no display driver exists
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
    ota_partition_family: OtaFamily::BareMetal, // memory-f746.x; full-image bundles

    // ---- provenance ----
    provenance: "firmware/ccid-firmware/src/board_config.rs f746::setup (IO=PI0, CLK=PF6, RST=PI2, PRES=PF10, PWR=PF7, USB PA11/PA12, backlight PK3); docs/MILESTONES.md M0-M3 Cherry ST-2xxx; design §2.3",
};

/// `stm32f746` — the Cargo feature that selects this table entry.
pub const FEATURE: &str = "stm32f746";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ALL_BOARDS;

    fn port_pin(gpio: u8) -> (char, u8) {
        ((b'A' + gpio / 16) as char, gpio % 16)
    }

    #[test]
    fn identity_and_family() {
        assert_eq!(F746.name, "stm32f746");
        assert_eq!(F746.chip, Chip::Stm32f746);
        assert_eq!(F746.chip.target(), None); // bare-metal leg
        assert_eq!(F746.ota_partition_family, OtaFamily::BareMetal);
        assert_eq!(FEATURE, "stm32f746");
        assert!(ALL_BOARDS.iter().any(|b| b.name == "stm32f746"));
    }

    #[test]
    fn smartcard_pins_match_the_constructor() {
        // board_config.rs f746::setup: IO=PI0, CLK=PF6, RST=PI2,
        // PRES=PF10, PWR=PF7.
        assert_eq!(port_pin(F746.pin("sc_io")), ('I', 0));
        assert_eq!(port_pin(F746.pin("sc_clk")), ('F', 6));
        assert_eq!(port_pin(F746.pin("sc_rst")), ('I', 2));
        assert_eq!(port_pin(F746.pin("sc_pres")), ('F', 10));
        assert_eq!(port_pin(F746.pin("sc_pwr")), ('F', 7));
    }

    #[test]
    fn usb_and_backlight_pins_match_the_constructor() {
        // board_config.rs f746: UsbDmPin = PA11<Alternate<10>>, DP = PA12;
        // _backlight: PK3<Output<PushPull>>.
        assert_eq!(port_pin(F746.pin("usb_dm")), ('A', 11));
        assert_eq!(port_pin(F746.pin("usb_dp")), ('A', 12));
        assert_eq!(port_pin(F746.pin("lcd_bl")), ('K', 3));
    }

    #[test]
    fn contact_only() {
        const {
            assert!(F746.caps.wired_contact);
        };
        assert_eq!(F746.nfc, None);
        assert!(F746.buses.is_empty());
        assert_eq!(F746.caps.lcd, Cap::None);
        assert_eq!(F746.wire, Wire::UsbCcid);
    }

    #[test]
    fn no_pin_double_claims() {
        let mut seen = std::collections::BTreeSet::new();
        for p in F746.pins {
            assert!(seen.insert(p.gpio), "pin {} double-claimed", p.gpio);
        }
    }
}
