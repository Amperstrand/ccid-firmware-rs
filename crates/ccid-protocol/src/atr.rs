use core::default::Default;

// osmo-ccid-firmware iso7816_3.c — "@implements ISO/IEC 7816-3:2006(E)
// table 7". Index 0 IS the Fi=372 default row: an ATR without TA1 means
// Fi=372, never Fi=0.
//
// ISO7816: const uint16_t iso7816_3_fi_table[16] = {
// ISO7816: 372, 372, 558, 744, 1116, 1488, 1860, 0,
// ISO7816: 0, 512, 768, 1024, 1536, 2048, 0, 0
// ISO7816: };
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtrParams {
    pub fi: u16,
    pub di: u8,
    pub ta1: u8,
    pub protocol: u8,
    pub guard_time_n: u8,
    pub ifsc: u8,
    pub bwi: u8,
    pub cwi: u8,
    pub edc_type: u8,
    pub has_ta1: bool,
}

// ATR-derived defaults per ISO 7816-3:2006 §8.3 (osmo iso7816_3.h:
// ISO7816_3_DEFAULT_FI 372, ISO7816_3_DEFAULT_DI 1) and §9.1/§9.5.3
// (T=1: IFSC default 32, BWI default 4, CWI default 13).
impl Default for AtrParams {
    fn default() -> Self {
        Self {
            fi: 372,
            di: 1,
            ta1: 0,
            protocol: 0,
            guard_time_n: 0,
            ifsc: 32,
            bwi: 4,
            cwi: 13,
            edc_type: 0,
            has_ta1: false,
        }
    }
}

pub fn fi_from_ta1_high(nibble: u8) -> u16 {
    const FI_TABLE: [u16; 16] = [
        372, 372, 558, 744, 1116, 1488, 1860, 0, 0, 512, 768, 1024, 1536, 2048, 0, 0,
    ];
    FI_TABLE.get(nibble as usize).copied().unwrap_or(372)
}

// ISO7816: const uint8_t iso7816_3_di_table[16] = {
// ISO7816: 0, 1, 2, 4, 8, 16, 32, 64,
// ISO7816: 12, 20, 0, 0, 0, 0, 0, 0,
// ISO7816: };
pub fn di_from_ta1_low(nibble: u8) -> u8 {
    const DI_TABLE: [u8; 16] = [0, 1, 2, 4, 8, 16, 32, 64, 12, 20, 0, 0, 0, 0, 0, 0];
    match DI_TABLE.get(nibble as usize).copied().unwrap_or(1) {
        0 => 1,
        x => x,
    }
}

pub fn parse_atr(atr: &[u8]) -> AtrParams {
    let mut p = AtrParams::default();
    if atr.len() < 2 {
        return p;
    }

    let t0 = atr[1];
    let mut y = (t0 >> 4) & 0x0F;
    let mut idx = 2usize;
    let mut level = 1u8;
    let mut td_protocol: u8 = 0;

    loop {
        if (y & 0x01) != 0 {
            if idx >= atr.len() {
                break;
            }
            let ta = atr[idx];
            idx += 1;
            if level == 1 {
                p.ta1 = ta;
                p.has_ta1 = true;
                p.fi = fi_from_ta1_high(ta >> 4);
                p.di = di_from_ta1_low(ta & 0x0F);
            } else if level >= 3 && td_protocol == 1 {
                p.ifsc = ta;
            }
        }

        if (y & 0x02) != 0 {
            if idx >= atr.len() {
                break;
            }
            let tb = atr[idx];
            idx += 1;
            if level >= 2 && td_protocol == 1 {
                p.bwi = (tb >> 4) & 0x0F;
                p.cwi = tb & 0x0F;
            }
        }

        if (y & 0x04) != 0 {
            if idx >= atr.len() {
                break;
            }
            let tc = atr[idx];
            idx += 1;
            if level == 1 {
                p.guard_time_n = tc;
            } else if td_protocol == 1 {
                p.edc_type = tc & 1;
            }
        }

        if (y & 0x08) != 0 {
            if idx >= atr.len() {
                break;
            }
            let td = atr[idx];
            idx += 1;
            td_protocol = td & 0x0F;
            if level == 1 {
                p.protocol = td_protocol;
            }
            y = (td >> 4) & 0x0F;
            level = match level.checked_add(1) {
                Some(l) => l,
                None => break,
            };
        } else {
            break;
        }
    }

    p
}

/// Verify TCK (check byte) for T=1 ATRs per ISO 7816-3 §8.2.4.
/// TCK is the XOR of all bytes from T0 to the byte before TCK.
/// Returns true if verification passes (or TCK is not required for T=0).
pub fn verify_atr_tck(atr: &[u8], protocol: u8) -> bool {
    if protocol != 1 {
        return true;
    }
    if atr.len() < 3 {
        return true;
    }
    let expected: u8 = atr[1..atr.len() - 1].iter().fold(0u8, |acc, &b| acc ^ b);
    expected == atr[atr.len() - 1]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcedureByte {
    Null,
    AckAll,
    AckOne,
    Status(u8),
    Unexpected(u8),
}

/// T=0 procedure-byte classification per ISO 7816-3:2006 §10.3.3:
/// 0x60 = NULL (wait), INS = ACK-all-remaining-data, INS ^ 0xFF = ACK-one
/// byte, 60-6F/90-9F ranges carry SW1.
/// Block wait time for a T=1 exchange start, in milliseconds: BWT =
/// 11 + 2^BWI · 960 etu (ISO 7816-3 §11.6.2.1) at the given wire baud
/// (etu = 1/baud). Ceiling division — the caller must wait at least as
/// long as the window it advertises to the host.
///
/// Shared single definition (fix-all-siblings): consumed by the STM32
/// relay (`smartcard_common`) and the esp32 T=1 endpoint (`t1.rs`).
pub fn t1_bwt_ms(bwi: u8, baud: u32) -> u32 {
    if baud == 0 {
        return 5_000;
    }
    let bwt_etu: u64 = 11 + (1u64 << (bwi as u32 & 0x0F)) * 960;
    let baud64 = baud as u64;
    let ms = (bwt_etu * 1_000).div_ceil(baud64);
    ms.min(u32::MAX as u64) as u32
}

/// The negotiated T=1 timing fields of a CCID §6.1.7
/// abProtocolDataStructure (T=1): byte 3 packs BWI (high) | CWI (low),
/// byte 5 is bIFSC. Single reference-locked home for the layout — used
/// by the contact-profile SetParameters application and the esp32
/// endpoint hook (G-04).
pub fn t1_bwi_cwi_ifsc(raw: &[u8; 7]) -> (u8, u8, u8) {
    (raw[3] >> 4, raw[3] & 0x0F, raw[5])
}

/// The TB3 byte encoding BWI/CWI for a T=1-declaring ATR (BWI high
/// nibble, CWI low nibble — the mirror of `t1_bwi_cwi_ifsc`).
pub const fn t1_tb3(bwi: u8, cwi: u8) -> u8 {
    (bwi << 4) | (cwi & 0x0F)
}

pub fn classify_t0_procedure_byte(ins: u8, pb: u8) -> ProcedureByte {
    if pb == 0x60 {
        return ProcedureByte::Null;
    }
    if pb == ins {
        return ProcedureByte::AckAll;
    }
    if pb == (ins ^ 0xFF) {
        return ProcedureByte::AckOne;
    }
    if (0x60..=0x6F).contains(&pb) || (0x90..=0x9F).contains(&pb) {
        return ProcedureByte::Status(pb);
    }
    ProcedureByte::Unexpected(pb)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationStep {
    AssertReset,
    EnablePower,
    EnableClock,
    ReleaseReset,
}

pub fn activation_plan() -> [ActivationStep; 4] {
    [
        ActivationStep::AssertReset,
        ActivationStep::EnablePower,
        ActivationStep::EnableClock,
        ActivationStep::ReleaseReset,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fi_mapping_values() {
        assert_eq!(fi_from_ta1_high(1), 372);
        assert_eq!(fi_from_ta1_high(2), 558);
        assert_eq!(fi_from_ta1_high(3), 744);
        assert_eq!(fi_from_ta1_high(9), 512);
        assert_eq!(fi_from_ta1_high(13), 2048);
    }

    #[test]
    fn test_di_mapping_values() {
        assert_eq!(di_from_ta1_low(1), 1);
        assert_eq!(di_from_ta1_low(2), 2);
        assert_eq!(di_from_ta1_low(3), 4);
        assert_eq!(di_from_ta1_low(8), 12);
        assert_eq!(di_from_ta1_low(9), 20);
    }

    #[test]
    fn test_parse_atr_t1_protocol() {
        let atr = [0x3B, 0x80, 0x01];
        let p = parse_atr(&atr);
        assert_eq!(p.protocol, 1);
    }

    #[test]
    fn test_t0_procedure_classification() {
        let ins = 0xA4;
        assert_eq!(classify_t0_procedure_byte(ins, 0x60), ProcedureByte::Null);
        assert_eq!(classify_t0_procedure_byte(ins, 0xA4), ProcedureByte::AckAll);
        assert_eq!(classify_t0_procedure_byte(ins, 0x5B), ProcedureByte::AckOne);
        assert_eq!(
            classify_t0_procedure_byte(ins, 0x90),
            ProcedureByte::Status(0x90)
        );
    }

    #[test]
    fn test_activation_order() {
        let plan = activation_plan();
        assert_eq!(
            plan,
            [
                ActivationStep::AssertReset,
                ActivationStep::EnablePower,
                ActivationStep::EnableClock,
                ActivationStep::ReleaseReset,
            ]
        );
    }

    #[test]
    fn test_tck_verification_t1_valid() {
        let atr = [0x3B, 0x80, 0x01, 0x81];
        let p = parse_atr(&atr);
        assert_eq!(p.protocol, 1);
        assert!(verify_atr_tck(&atr, p.protocol));
    }

    #[test]
    fn test_tck_verification_t1_invalid() {
        let atr = [0x3B, 0x80, 0x01, 0xFF];
        let p = parse_atr(&atr);
        assert_eq!(p.protocol, 1);
        assert!(!verify_atr_tck(&atr, p.protocol));
    }

    #[test]
    fn test_tck_verification_t0_skipped() {
        let atr = [0x3B, 0x00, 0xFF];
        let p = parse_atr(&atr);
        assert_eq!(p.protocol, 0);
        assert!(verify_atr_tck(&atr, p.protocol));
    }

    #[test]
    fn test_parse_atr_random_bytes_no_panic() {
        let mut rng = crate::testutil::Xorshift32(0xDEAD_BEEF);
        let mut buf = [0u8; 33];
        for _ in 0..1000 {
            let len = (rng.next_u32() as usize) % 34;
            rng.fill_bytes(&mut buf);
            let _ = parse_atr(&buf[..len]);
        }
    }

    #[test]
    fn test_parse_atr_empty() {
        let p = parse_atr(&[]);
        assert_eq!(p, AtrParams::default());
    }

    #[test]
    fn test_parse_atr_single_byte() {
        let p = parse_atr(&[0x3B]);
        assert_eq!(p, AtrParams::default());
    }

    #[test]
    fn test_parse_atr_malformed_td_chain() {
        // TS=0x3B, T0=0x89 (Y1: TA1+TD1 present, no TB1/TC1)
        // TA1=0x11, TD1=0x81 (Y2: TA2 present, T=1)
        // Truncated at 4 bytes — TA2 is missing.
        let atr = [0x3B, 0x89, 0x11, 0x81];
        let p = parse_atr(&atr);
        // Should not panic; protocol=1 from TD1.
        assert_eq!(p.protocol, 1);
    }

    #[test]
    fn test_verify_tck_random_bytes_no_panic() {
        let mut rng = crate::testutil::Xorshift32(0xCAFE_BABE);
        let mut buf = [0u8; 64];
        for _ in 0..1000 {
            let len = (rng.next_u32() as usize) % 65;
            let protocol = rng.next_u8();
            rng.fill_bytes(&mut buf);
            let _ = verify_atr_tck(&buf[..len], protocol);
        }
    }

    #[test]
    fn test_verify_tck_empty_atr() {
        assert!(verify_atr_tck(&[], 1));
    }

    #[test]
    fn test_procedure_byte_all_values() {
        for pb in 0..=255u8 {
            let result = classify_t0_procedure_byte(0xA4, pb);
            match result {
                ProcedureByte::Null
                | ProcedureByte::AckAll
                | ProcedureByte::AckOne
                | ProcedureByte::Status(_)
                | ProcedureByte::Unexpected(_) => {}
            }
        }
    }

    #[test]
    fn test_fi_di_table_complete() {
        // Known values from ISO 7816-3
        assert_eq!(fi_from_ta1_high(1), 372);
        assert_eq!(fi_from_ta1_high(2), 558);
        assert_eq!(di_from_ta1_low(1), 1);
        assert_eq!(di_from_ta1_low(2), 2);

        // All 16 nibble values must not panic
        for i in 0..16u8 {
            let _ = fi_from_ta1_high(i);
            let _ = di_from_ta1_low(i);
        }
    }

    /// GREAT EXPECTATION lock: the full Fi table must stay byte-identical
    /// to the reference (osmo iso7816_3.c `iso7816_3_fi_table`,
    /// @implements ISO 7816-3:2006 Table 7). Index 0 is the Fi=372
    /// default row — NOT zero (audit finding #4: a 0 here gave Fi=0 for
    /// TA1-less ATRs, corrupting ETU math downstream). If this breaks,
    /// re-verify against the reference — never "fix" the test.
    #[test]
    fn fi_table_locked_to_iso7816_reference() {
        let expected: [u16; 16] = [
            372, 372, 558, 744, 1116, 1488, 1860, 0, 0, 512, 768, 1024, 1536, 2048, 0, 0,
        ];
        for nib in 0..16u8 {
            assert_eq!(
                fi_from_ta1_high(nibble_u4(nib)),
                expected[nibble_u4(nib) as usize],
                "Fi mismatch at TA1 high nibble {}",
                nib
            );
        }
    }

    /// GREAT EXPECTATION lock: Di table byte-identical to osmo
    /// `iso7816_3_di_table` (ISO 7816-3:2006 Table 8); reserved nibbles
    /// (Di=0) fall back to Di=1 rather than dividing by zero.
    #[test]
    fn di_table_locked_to_iso7816_reference() {
        let expected: [u8; 16] = [1, 1, 2, 4, 8, 16, 32, 64, 12, 20, 1, 1, 1, 1, 1, 1];
        for nib in 0..16u8 {
            assert_eq!(
                di_from_ta1_low(nibble_u4(nib)),
                expected[nibble_u4(nib) as usize],
                "Di mismatch at TA1 low nibble {}",
                nib
            );
        }
    }

    /// TA1 high nibble 0 = the Fi=372 default row (ISO Table 7 index 0).
    #[test]
    fn fi_index_zero_is_default_372() {
        assert_eq!(fi_from_ta1_high(0), 372);
        assert_eq!(AtrParams::default().fi, 372);
        // An ATR with TA1 high nibble 0 keeps the default Fi.
        let p = parse_atr(&[0x3B, 0x90, 0x0F, 0x00]);
        assert_eq!(p.fi, 372);
        assert!(p.has_ta1);
    }

    fn nibble_u4(v: u8) -> u8 {
        v & 0x0F
    }

    #[test]
    fn test_parse_atr_overflow_td_chain() {
        let mut atr = [0u8; 302];
        atr[0] = 0x3B;
        atr[1] = 0x88;
        for i in 0..300 {
            atr[2 + i] = 0x80;
        }
        let p = parse_atr(&atr);
        // Should not panic; returns partial result from TD1 (protocol=0)
        assert_eq!(p.protocol, 0);
    }

    #[test]
    fn t1_bwt_ms_matches_iso_7816_3_formula() {
        // BWI=4, 11290 baud (4.2 MHz / 372): (11 + 16·960) etu / 11290
        // = 1361.47 ms → 1362 (ceiling). This is the window we echo to
        // the host as BWI=4 — the reader must honor it (#132: the old
        // hardcoded 500 ms raced legal BWT-class answers).
        assert_eq!(t1_bwt_ms(4, 11_290), 1_362);
        // BWI=0, 10752 baud (4 MHz / 372): (11 + 960) etu / 10752 = 90.3 → 91.
        assert_eq!(t1_bwt_ms(0, 10_752), 91);
        // BWI=4 at the esp32's advertised default (G-05 TB3): ~1.43 s.
        assert_eq!(t1_bwt_ms(4, 10_752), 1_430);
    }

    #[test]
    fn t1_bwt_ms_is_monotonic_in_bwi_and_safe_at_edges() {
        assert!(t1_bwt_ms(5, 11_290) > t1_bwt_ms(4, 11_290));
        assert!(t1_bwt_ms(15, 11_290) > t1_bwt_ms(4, 11_290));
        assert_eq!(t1_bwt_ms(4, 0), 5_000);
    }

    #[test]
    fn t1_bwi_cwi_ifsc_and_tb3_round_trip() {
        let raw = [0x11, 0x00, 0x00, 0x45, 0x00, 0xFE, 0x00];
        assert_eq!(t1_bwi_cwi_ifsc(&raw), (4, 5, 254));
        assert_eq!(t1_tb3(4, 5), 0x45);
        assert_eq!(t1_tb3(0, 3), 0x03);
    }
}
