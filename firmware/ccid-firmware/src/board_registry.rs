//! boards-as-data registry line (design §4 step 4): FWID/board name
//! ONLY — the STM32 wiring stays in the Layer-B constructor
//! (`board_config.rs`, the bin tree) where AF-mode type-state is
//! compiler-checked. This is the one line that ties a ccid-firmware
//! build to its `crates/boards` Layer-A row.

use boards::BoardDesc;

/// The board this firmware build targets — selected by the same Cargo
/// feature that selects the constructor (mutually exclusive, enforced
/// by the feature graph). Host tests assert name/feature agreement.
#[cfg(feature = "stm32f469")]
pub const BOARD: &BoardDesc = &boards::F469;

#[cfg(feature = "stm32f746")]
pub const BOARD: &BoardDesc = &boards::F746;

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry line agrees with the selected feature (the default
    /// build is f469 — CI also builds the f746 combo; each asserts its
    /// own name here).
    #[cfg(feature = "stm32f469")]
    #[test]
    fn registry_matches_the_selected_board() {
        assert_eq!(BOARD.name, "stm32f469");
        assert_eq!(boards::stm32f469::FEATURE, "stm32f469");
        assert!(BOARD.caps.wired_contact);
    }

    #[cfg(feature = "stm32f746")]
    #[test]
    fn registry_matches_the_selected_board() {
        assert_eq!(BOARD.name, "stm32f746");
        assert_eq!(boards::stm32f746::FEATURE, "stm32f746");
        assert!(BOARD.caps.wired_contact);
    }

    /// Bare-metal boards carry no OTA slots (design §2.3): full-image
    /// flash bundles only.
    #[test]
    fn bare_metal_ota_family() {
        assert_eq!(BOARD.ota_partition_family, boards::OtaFamily::BareMetal);
    }
}
