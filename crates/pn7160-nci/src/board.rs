//! #149 P2 — board geometry for the PN7160 block. The protocol core
//! (`transport.rs`, `driver.rs`) carries NO board constants; every board
//! describes itself with a [`BoardPins`], and the nucula reference is
//! the provided default. Pure data — host-testable, no esp deps.

/// Board geometry for the PN7160 block.
///
/// Board rules (nucula schematic + bench): I2C ≤ 100 kHz (PCF8574T
/// limit on the shared bus), EXTERNAL 2.2 kΩ pull-ups to 3.0 V — ESP
/// internal pull-ups must stay OFF — and R23/R24 strap the NCI address
/// (probe before assuming; strap margin confirmed on the bench, #62):
/// nominal 0x28.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardPins {
    /// NCI I2C address (R23/R24 strap; nominal 0x28 — probe first).
    pub addr: u8,
    /// SDA GPIO number.
    pub sda: i32,
    /// SCL GPIO number.
    pub scl: i32,
    /// IRQ GPIO number (level-high until read).
    pub irq: i32,
    /// VEN power-control GPIO number.
    pub ven: i32,
}

/// The nucula (ESP32-C3 + PN7160) reference board — the historical
/// constants as ONE describable data point instead of the crate's
/// baked-in geometry (#149 P2).
pub const NUCULA: BoardPins = BoardPins {
    addr: 0x28,
    sda: 4,
    scl: 5,
    irq: 6,
    ven: 7,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// #149 P2 lock: the nucula reference geometry lives HERE (one
    /// describable data point), and the protocol core carries none of
    /// it — the transport.rs `pin_constants_match_board` assertions,
    /// relocated with the constants.
    #[test]
    fn nucula_default_matches_the_historical_constants() {
        assert_eq!(NUCULA.addr, 0x28);
        assert_eq!(NUCULA.sda, 4);
        assert_eq!(NUCULA.scl, 5);
        assert_eq!(NUCULA.irq, 6);
        assert_eq!(NUCULA.ven, 7);
    }

    /// A second board is describable without touching the crate.
    #[test]
    fn a_second_board_is_pure_data() {
        let other = BoardPins {
            addr: 0x29,
            sda: 8,
            scl: 9,
            irq: 10,
            ven: 11,
        };
        assert_ne!(other, NUCULA);
    }
}
