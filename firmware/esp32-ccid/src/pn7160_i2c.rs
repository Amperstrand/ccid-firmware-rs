//! #149 P1: the transport moved to `pn7160_nci::esp_transport` (shared
//! crate, opt-in `esp` feature) — this module is the compatibility
//! re-export plus the ONE ccid-specific piece (the NfcDriver alias),
//! which stays in the firmware per the extraction boundary.

pub use pn7160_nci::esp_transport::*;

use crate::pn7160_driver::Pn7160NfcDriver;

/// Assembled NfcDriver over the real I²C link.
pub type EspPn7160NfcDriver = Pn7160NfcDriver<EspPn7160Transport>;
