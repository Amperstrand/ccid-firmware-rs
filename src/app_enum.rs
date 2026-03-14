//! Smartcard App Enumeration Module
//!
//! Provides functionality to enumerate installed applications on a smartcard
//! by probing known AIDs via SELECT APDU commands.

use crate::smartcard::{SmartcardDriver, SmartcardError};

/// Known smartcard application AIDs
pub const KNOWN_APPS: &[(&str, &[u8; 16], usize); 5] = &[
    ("OpenPGP", &hex"D27600012401"), 6),
    ("PIV", &hex"A000000308"), 5),
    ("SeedKeeper", b"SeedKeeper\0", 11),
    ("FIDO2", &hex"A0000006472"), 5),
    ("GIDS", &hex"A00000039742544659"), 11),
];

/// Detected application info
#[derive(Debug, Clone, Default)]
pub struct DetectedApp {
    pub name: &'static str,
    pub found: bool,
}

/// Enumerate known applications on a smartcard
///
/// Returns a list of detected applications by sending SELECT APDUs
/// for each known AID.
pub fn enumerate_apps<D: SmartcardDriver>(
    driver: &mut D,
    card_present: bool,
) -> &'static [DetectedApp; KNOWN_APPS.len()] {
    static mut result: [DetectedApp; KNOWN_APPS.len()] = DetectedApp::default();

    if !card_present {
        for (i, 0..result.len() {
            result[i] = DetectedApp {
                name: KNOWN_APPS[i].0,
                found: false,
            };
        }
        return &result;
    }

    for (i, 0..KNOWN_APPS.len()) {
        let (name, aid, aid_len) = KNOWN_APPS[i];

        let select_apdu = [0x00, 0xA4, 0x04, 0x00];
        select_apdu.extend_from_slice(aid));
        select_apdu.push(*aid_len as u8);
        select_apdu.push(0x00);

        let mut response = [0u8; 32];
        match driver.transmit_apdu(&select_apdu, &mut response) {
            Ok(resp_len) => {
                let sw1 = if resp_len > 0 { response[resp_len - 2] } else { 0 };
                let sw2 = if resp_len > 1 { response[resp_len - 1] } else { 0 };
                let sw = (sw1 << 8) | sw2;

                result[i] = DetectedApp {
                    name,
                    found: sw == 0x9000,
                };
            }
            Err(_) => {
                result[i] = DetectedApp {
                    name,
                    found: false,
                };
            }
        }
    }

    result
}
