//! Smartcard App Enumeration Module
//!
//! Provides functionality to enumerate installed applications on a smartcard
//! by probing known AIDs via SELECT APDU commands.
//!
//! Only available when the `display` feature is enabled.

#![cfg(feature = "display")]

use crate::ccid::SmartcardDriver;

/// Number of known applications
pub const APP_COUNT: usize = 5;

/// OpenPGP AID
const AID_OPENPGP: [u8; 6] = [0xD2, 0x76, 0x00, 0x01, 0x24, 0x01];
/// PIV AID
const AID_PIV: [u8; 5] = [0xA0, 0x00, 0x00, 0x03, 0x08];
/// SeedKeeper AID
const AID_SEEDKEEPER: [u8; 11] = [
    b'S', b'e', b'e', b'd', b'K', b'e', b'e', b'p', b'e', b'r', 0x00,
];
/// FIDO2 AID
const AID_FIDO2: [u8; 5] = [0xA0, 0x00, 0x00, 0x06, 0x47];
/// GIDS AID
const AID_GIDS: [u8; 11] = [
    0xA0, 0x00, 0x00, 0x03, 0x97, 0x42, 0x54, 0x46, 0x59, 0x00, 0x00,
];

/// Known smartcard application AIDs
pub const KNOWN_APPS: [(&str, &[u8], usize); APP_COUNT] = [
    ("OpenPGP", &AID_OPENPGP, 6),
    ("PIV", &AID_PIV, 5),
    ("SeedKeeper", &AID_SEEDKEEPER, 11),
    ("FIDO2", &AID_FIDO2, 5),
    ("GIDS", &AID_GIDS, 11),
];

/// Detected application info
#[derive(Debug, Clone, Copy, Default)]
pub struct DetectedApp {
    pub name: &'static str,
    pub found: bool,
}

/// State for app enumeration
pub struct AppEnumerationState {
    /// Detected applications
    apps: [DetectedApp; APP_COUNT],
    /// Whether enumeration has been done
    enumerated: bool,
}

impl Default for AppEnumerationState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppEnumerationState {
    /// Create a new enumeration state with default values
    #[allow(clippy::needless_range_loop)]
    pub fn new() -> Self {
        let mut apps = [DetectedApp::default(); APP_COUNT];
        for i in 0..APP_COUNT {
            apps[i] = DetectedApp {
                name: KNOWN_APPS[i].0,
                found: false,
            };
        }
        Self {
            apps,
            enumerated: false,
        }
    }

    /// Reset enumeration state (call on card removal)
    #[allow(clippy::needless_range_loop)]
    pub fn reset(&mut self) {
        for i in 0..APP_COUNT {
            self.apps[i].found = false;
        }
        self.enumerated = false;
    }

    /// Run enumeration if needed and card is present
    /// Returns true if enumeration was performed
    #[allow(clippy::needless_range_loop)]
    pub fn enumerate_if_needed<D: SmartcardDriver>(
        &mut self,
        driver: &mut D,
        card_present: bool,
    ) -> bool {
        if !card_present {
            self.reset();
            return false;
        }

        if self.enumerated {
            return false;
        }

        // Perform enumeration
        for i in 0..APP_COUNT {
            let (name, aid, aid_len) = KNOWN_APPS[i];

            // Build SELECT APDU: CLA INS P1 P2 Lc [AID] Le
            // APDU: 00 A4 04 00 Lc [AID bytes] 00
            let mut apdu = [0u8; 32];
            apdu[0] = 0x00; // CLA
            apdu[1] = 0xA4; // INS = SELECT
            apdu[2] = 0x04; // P1 = select by AID
            apdu[3] = 0x00; // P2
            apdu[4] = aid_len as u8; // Lc = AID length

            // Copy AID bytes
            let len = aid_len.min(26);
            apdu[5..(5 + len)].copy_from_slice(&aid[..len]);
            apdu[5 + len] = 0x00; // Le

            let apdu_total_len = 5 + len + 1;

            let mut response = [0u8; 32];
            match driver.transmit_apdu(&apdu[..apdu_total_len], &mut response) {
                Ok(resp_len) => {
                    let sw1 = if resp_len > 1 {
                        response[resp_len - 2]
                    } else {
                        0
                    };
                    let sw2 = if resp_len > 0 {
                        response[resp_len - 1]
                    } else {
                        0
                    };
                    let sw = (sw1 as u16) << 8 | (sw2 as u16);

                    self.apps[i] = DetectedApp {
                        name,
                        found: sw == 0x9000,
                    };
                }
                Err(_) => {
                    self.apps[i] = DetectedApp { name, found: false };
                }
            }
        }

        self.enumerated = true;
        true
    }

    /// Get iterator over detected app names
    pub fn detected_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.apps.iter().filter(|app| app.found).map(|app| app.name)
    }

    /// Check if enumeration has been done
    pub fn is_enumerated(&self) -> bool {
        self.enumerated
    }
}
