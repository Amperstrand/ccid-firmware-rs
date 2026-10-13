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
    fn send(&mut self, _cmd: &[u8]) -> bool {
        false // wired to esp-idf-sys in the firmware crate
    }
    fn poll_frame(&mut self) -> Option<Frame> {
        None
    }
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

/// PC/SC ATR from ISO 14443-4 ATS (PC/SC Part 3 §4.2.3.3, contactless
/// ATR construction rules).
///
/// ISO 14443-4 ATS: TL(1) | T0(1) | TA1/TB1/TC1 (per T0's Y-mask) |
/// historical bytes (T0's K count) | TL-checksum. The ATR strips:
/// - TL — the ATS length byte is replaced by the ATR TS convention
///   marker 0x3B (direct convention)
/// - CRC_A — the ISO 14443-3 transport checksum never appears in the
///   contact-side ATR
/// The resulting ATR carries the ATS interface bytes verbatim: T0
/// becomes the ATR's T0 (Y1 mask + historical count), TA1/TB1/TC1
/// become the ATR's interface group, and the historical bytes pass
/// through. pcscd parses these to derive Fi/Di and protocol selection.
/// Build a PC/SC-compatible ATR from an ISO 14443-4 ATS.
///
/// The ATS interface bytes (TA1/TB1/TC1: D/F coding, SFGT, IFSC) are NOT
/// ATR interface bytes — copying them verbatim produced a malformed ATR
/// (T0's low nibble read as a historical-byte count of the FSCI,
/// TA1=0x77 is not a valid ATR Fi/Di, no TD1 → T=0 claim, no TCK) that
/// pcscd's parser rejects outright: bench 2026-10-11, the broker wire
/// log showed IccPowerOn answered with a fine ATR-shaped frame and
/// pcscd then sent NO SetParameters/PPS and failed the connect with
/// "Card is unresponsive".
///
/// G-05 shape (f8a897f, mirroring the m5stick builder's fix): TD1
/// carries the T=1 nibble — libccid's ATR_GetDefaultProtocol reads the
/// protocol from TD1, so TD1=0x80 left T=0 as the first offer (the
/// PROTO_MISMATCH class). TA3=0xFE sets IFSC=254 (the host would chain
/// every block at 32 otherwise — 8x the round trips) and TB3=0x45 sets
/// BWI=4/CWI=5: TS=3B, T0=0x80|n_hist, TD1=0x81, TD2=0x31, TA3=0xFE,
/// TB3=0x45, historical bytes, TCK.
///
/// Input contract: the ATS as carried by the NCI RF_INTF_ACTIVATED
/// notification — [T0, interface bytes..., historical bytes...] with no
/// TL prefix and no CRC_A (bench fixture `auto_activation_ntf`,
/// 2026-10-11: the P71's ATS arrives as 78 77 91 02 80 73 C8 21 10).
pub fn ats_to_atr(ats: &[u8]) -> Option<Vec<u8, 32>> {
    if ats.is_empty() {
        return None;
    }
    // ISO 14443-4 §5.1.1 T0: high nibble = interface-byte presence mask
    // (b8=TA1, b7=TB1, b6=TC1, b5=TD1; each consumed TD's high nibble is
    // the next mask).
    let mut i = 1;
    let mut y = ats[0] & 0xF0;
    while y != 0 {
        let mut next = 0;
        for bit in [0x80u8, 0x40, 0x20, 0x10] {
            if y & bit != 0 {
                if i >= ats.len() {
                    return None;
                }
                let b = ats[i];
                i += 1;
                if bit == 0x10 {
                    next = b & 0xF0;
                }
            }
        }
        y = next;
    }
    let hist = &ats[i..];
    let mut atr = Vec::new();
    let _ = atr.push(0x3B);
    let _ = atr.push(0x80 | (hist.len() as u8 & 0x0F));
    let _ = atr.push(0x81);
    let _ = atr.push(0x31);
    let _ = atr.push(0xFE);
    // TB3 inline (BWI=4, CWI=5): the crate stays dependency-free for
    // the bolty-rs consumer.
    let _ = atr.push(0x45);
    atr.extend_from_slice(hist).ok()?;
    let mut tck = 0u8;
    for &b in &atr[1..] {
        tck ^= b;
    }
    let _ = atr.push(tck);
    Some(atr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ats_to_atr_t1_shape_and_tck() {
        // P71 ATS as delivered by the nucula activation NTF (bench
        // fixture, 2026-10-11): T0=0x78 (TB1/TC1/TD1, FSCI=8), then
        // 77 91 02, historical 80 73 C8 21 10. The built ATR is
        // byte-identical to the m5stick relay's bench-proven one.
        let ats = [0x78, 0x77, 0x91, 0x02, 0x80, 0x73, 0xC8, 0x21, 0x10];
        let atr = ats_to_atr(&ats).expect("conversion");
        assert_eq!(
            atr.as_slice(),
            &[0x3B, 0x85, 0x81, 0x31, 0xFE, 0x45, 0x80, 0x73, 0xC8, 0x21, 0x10, 0x84]
        );
    }

    #[test]
    fn ats_to_atr_td_chaining() {
        // T0=0x75 (TB1/TC1/TD1 present), TD1=0x82 chains a TA2 round;
        // two historical bytes.
        let ats = [0x75, 0x77, 0x81, 0x82, 0x88, 0x31, 0x32];
        let atr = ats_to_atr(&ats).expect("conversion");
        assert_eq!(
            atr.as_slice(),
            &[0x3B, 0x82, 0x81, 0x31, 0xFE, 0x45, 0x31, 0x32, 0x8A]
        );
    }

    #[test]
    fn ats_to_atr_no_interface_bytes() {
        // T0=0x10: only TD1 (0x00 → no chain) — historical only.
        let ats = [0x10, 0x00, 0x31, 0x32, 0x33];
        let atr = ats_to_atr(&ats).expect("conversion");
        assert_eq!(
            atr.as_slice(),
            &[0x3B, 0x83, 0x81, 0x31, 0xFE, 0x45, 0x31, 0x32, 0x33, 0xB8]
        );
    }

    #[test]
    fn ats_to_atr_empty_history() {
        let ats = [0x78, 0x77, 0x91, 0x02];
        let atr = ats_to_atr(&ats).expect("conversion");
        assert_eq!(atr.as_slice(), &[0x3B, 0x80, 0x81, 0x31, 0xFE, 0x45, 0x8B]);
    }

    #[test]
    fn ats_to_atr_minimal() {
        // T0=0x40 (TB1 only), no historical bytes.
        let atr = ats_to_atr(&[0x40, 0x77]).expect("conversion");
        assert_eq!(atr.as_slice(), &[0x3B, 0x80, 0x81, 0x31, 0xFE, 0x45, 0x8B]);
    }

    #[test]
    fn ats_to_atr_empty() {
        assert!(ats_to_atr(&[]).is_none());
        // A lone T0 (mask 0x00, no interface bytes, no history) still
        // synthesizes a valid 5-byte ATR.
        assert!(ats_to_atr(&[0x01]).is_some());
    }

    #[test]
    fn ats_to_atr_garbage_interface_mask_rejected() {
        // T0 promises interface bytes the buffer cannot hold.
        assert!(ats_to_atr(&[0x78, 0x77]).is_none());
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
