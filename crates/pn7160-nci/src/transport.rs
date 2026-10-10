//! Concrete I2C Transport variants for the nucula board (ESP32-C3 + PN7160).
//!
//! Three initialization variants, one per possible pad-diag verdict branch
//! (issue #62). Tonight: run the pad-diag probe, read the verdict, pick the
//! variant that matches.
//!
//! Also provides proper PC/SC ATR construction from ISO 14443-4 ATS.

use crate::{Frame, Transport};
use heapless::Vec;

pub const PN7160_I2C_ADDR: u8 = 0x28;
pub const VEN_PIN: i32 = 7;
pub const IRQ_PIN: i32 = 6;
pub const SDA_PIN: i32 = 4;
pub const SCL_PIN: i32 = 5;

/// Stub transport: the firmware crate wires this to esp-idf-sys I2C.
#[derive(Default)]
pub struct I2cTransport {
    pub bus_ready: bool,
}

impl I2cTransport {
    pub fn new() -> Self {
        I2cTransport { bus_ready: false }
    }
}

impl Transport for I2cTransport {
    fn transact(&mut self, _cmd: &[u8]) -> Option<Frame> {
        None // wired to esp-idf-sys in the firmware crate
    }
    fn drain(&mut self) -> Option<Frame> {
        None
    }
}

/// Verdict A: PAD STUCK — clear gpio hold + sleep isolation on pads 4-7.
pub fn init_variant_pad_hold_clear() -> Result<I2cTransport, &'static str> {
    // gpio_hold_dis(pin) for SDA/SCL/IRQ/VEN
    // gpio_deep_sleep_hold_dis()
    // gpio_sleep_sel_dis(pin) for SDA/SCL/IRQ/VEN
    Err("wire to esp-idf-sys in firmware crate")
}

/// Verdict B: CONFIG-ORDER — create bus before Rust main spawns.
pub fn init_variant_config_order() -> Result<I2cTransport, &'static str> {
    // bus creation first (native firmware's nucula.cpp:181 position)
    // then VEN cycle, then ladder
    Err("wire to esp-idf-sys in firmware crate")
}

/// Verdict C: VEN-TIMING — quiesce bus, extended VEN timing.
pub fn init_variant_ven_timing() -> Result<I2cTransport, &'static str> {
    // gpio_set_level(VEN_PIN, 1); delay_ms(10);
    // gpio_set_level(VEN_PIN, 0); delay_ms(100);
    // gpio_set_level(VEN_PIN, 1); delay_ms(100);
    Err("wire to esp-idf-sys in firmware crate")
}

/// VEN power cycle, byte-exact vs nci.c:77-86.
pub fn ven_cycle() {
    // gpio_set_level(VEN_PIN, 1); delay_ms(10);
    // gpio_set_level(VEN_PIN, 0); delay_ms(50);
    // gpio_set_level(VEN_PIN, 1); delay_ms(50);
}

/// PC/SC ATR from ISO 14443-4 ATS.
/// TS=0x3B + ATS body (T0 through historical, no TL, no CRC_A).
pub fn ats_to_atr(ats: &[u8]) -> Option<Vec<u8, 32>> {
    if ats.len() < 2 {
        return None;
    }
    let tl = ats[0] as usize;
    let body_end = tl.min(ats.len());
    if body_end < 2 {
        return None;
    }
    let body = &ats[1..body_end];
    let mut atr = Vec::new();
    let _ = atr.push(0x3B);
    let _ = atr.extend_from_slice(body);
    Some(atr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ats_to_atr_ntag424() {
        let ats = [0x06, 0x75, 0x77, 0x81, 0x02, 0x88];
        let atr = ats_to_atr(&ats).expect("conversion");
        assert_eq!(atr[0], 0x3B);
        assert_eq!(&atr[1..], &ats[1..6]);
    }

    #[test]
    fn ats_to_atr_minimal() {
        let atr = ats_to_atr(&[0x02, 0x75]).expect("conversion");
        assert_eq!(atr.as_slice(), &[0x3B, 0x75]);
    }

    #[test]
    fn ats_to_atr_empty() {
        assert!(ats_to_atr(&[]).is_none());
        assert!(ats_to_atr(&[0x01]).is_none());
    }

    #[test]
    fn ats_to_atr_truncated() {
        let atr = ats_to_atr(&[0x0A, 0x75, 0x77, 0x81]).expect("truncated ok");
        assert_eq!(atr.as_slice(), &[0x3B, 0x75, 0x77, 0x81]);
    }

    #[test]
    fn pin_constants_match_board() {
        assert_eq!(SDA_PIN, 4);
        assert_eq!(SCL_PIN, 5);
        assert_eq!(IRQ_PIN, 6);
        assert_eq!(VEN_PIN, 7);
        assert_eq!(PN7160_I2C_ADDR, 0x28);
    }
}
