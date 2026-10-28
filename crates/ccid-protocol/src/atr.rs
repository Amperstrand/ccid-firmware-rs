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

/// Reader first-byte patience floor for a T=1 exchange (#137): the
/// historical pre-derivation value — BWTs below it (BWI 0-2 at bench
/// bauds) still wait 500 ms, preserving the bench-proven #132
/// turnaround margin.
pub const T1_FIRST_BYTE_FLOOR_MS: u32 = 500;

/// Reader first-byte patience ceiling for a T=1 exchange (#137).
///
/// THE CAP IS A FORCED, TESTED DECISION, not an assumption: the reader
/// waits at least the BWT it advertises (GetParameters echo) for every
/// BWI whose BWT fits under this ceiling. Above it the reader
/// saturates and is LESS patient than advertised — the cross-over sits
/// at BWI = 6 for the 10752 default baud (BWT 5716 ms) and at 9600 —
/// accepted because a single-threaded reader cannot block its
/// serve loop for multi-second/minute BWTs without CCID time-extension
/// signaling, and time extension is an explicit #137 NON-GOAL ("no
/// CCID time-extension signaling without a separate design pass").
/// The invariant tests below break loudly if the formula or the cap
/// moves; re-derive against the reference, never delete them.
pub const T1_FIRST_BYTE_MAX_MS: u32 = 5_000;

/// First-prologue-byte wait policy for a T=1 exchange (#137): the
/// advertised BWT (`t1_bwt_ms`) at the CURRENT wire baud, floored and
/// capped per the constants above. Single definition — the STM32
/// USART relay calls this instead of re-deriving.
pub fn t1_first_byte_timeout_ms(bwi: u8, baud: u32) -> u32 {
    t1_bwt_ms(bwi, baud).clamp(T1_FIRST_BYTE_FLOOR_MS, T1_FIRST_BYTE_MAX_MS)
}

/// Character wait time within a T=1 block, in milliseconds: CWT =
/// (2^CWI + 11) etu (ISO 7816-3 §11.6.4, triangulated in
/// docs/T1_BEST_PRACTICES.md §2.4: default CWI 13 ≈ 727 ms at the
/// 11290 reference baud, CWI 0 → 12 etu ≈ 1.06 ms; "CWT is *smaller*
/// than BWT — a slow-but-alive card between bytes is a WTX case, not
/// a CWT violation"). Ceiling division — same at-least-as-patient
/// contract as `t1_bwt_ms`.
pub fn t1_cwt_ms(cwi: u8, baud: u32) -> u32 {
    if baud == 0 {
        return T1_INTER_BYTE_FLOOR_MS;
    }
    let cwt_etu: u64 = (1u64 << (cwi as u32 & 0x0F)) + 11;
    let ms = (cwt_etu * 1_000).div_ceil(baud as u64);
    ms.min(u32::MAX as u64) as u32
}

/// Reader inter-byte patience floor for a T=1 exchange (#137): the
/// bench-proven #132 flat value — ~47× margin over a default-class
/// CWT (≈ 1-4 ms), so low-CWI cards keep today's exact timing.
pub const T1_INTER_BYTE_FLOOR_MS: u32 = 50;

/// Inter-byte wait policy for a T=1 exchange (#137): the advertised
/// CWT (`t1_cwt_ms`) floored at the historical 50 ms and capped at the
/// same 5 s ceiling as the first-byte wait. More inter-byte patience
/// never corrupts a block — it only delays the truncation error on a
/// genuinely dead card; LESS patience than the advertised CWT would
/// time out legal high-CWI cards mid-block.
pub fn t1_inter_byte_timeout_ms(cwi: u8, baud: u32) -> u32 {
    t1_cwt_ms(cwi, baud).clamp(T1_INTER_BYTE_FLOOR_MS, T1_FIRST_BYTE_MAX_MS)
}

/// Character spacing the pre-#135 F469 wire produced, in HALF-ETU units
/// (×2, so 24 = 12 ETU): STOP=1.0 + GT=1 → 12 ETU per character —
/// coincidentally the ISO N=2 spacing (10 data ETU + 2 guard). This is
/// the framing the measured 42.8 ms p50 bench number ran (#122
/// decomposition).
pub const T1_CHAR_HALF_ETU_LEGACY_N2: u32 = 24;

/// Character spacing upper bound of the post-#135 conservative framing,
/// in HALF-ETU units (27 = 13.5 ETU): STOP=1.5 (11.5 ETU frame) + GTPR
/// GT=2. The exact RM0090 GT-counting semantics are pending the
/// owner-PDF pass (see reference/ST/PROVENANCE.md); anything in
/// [24, 27] half-ETU is ISO-legal.
pub const T1_CHAR_HALF_ETU_POST_135_MAX: u32 = 27;

/// Wire time of one complete T=1 exchange — TX block of `tx_chars`
/// characters followed by the RX block of `rx_chars` — at Fi/Di on a
/// card clock of `card_clk_hz`, in whole milliseconds (ceiling).
/// `char_half_etu` is the per-character spacing in half-ETU units (see
/// the constants above).
///
/// This is the #122 latency-decomposition model: wire_ms =
/// (tx + rx) · char_half_etu/2 · Fi / (Di · clk). It is deliberately
/// pure math with no I/O so the host unit tests can lock the arithmetic
/// the 42.8 ms → ~11 ms gap analysis rests on: at the bench wire
/// (Fi=372, Di=1, clk=4.2 MHz — pclk1 42 MHz / prescaler 5) a 17+6-char
/// SELECT round trip spends ~24 ms ON THE WIRE, and the same workload
/// at a PPS-negotiated Di=8 spends ~3 ms. Baud derivation is the same
/// expression the reader applies (`baud = clk · Di / Fi`).
pub fn t1_exchange_wire_time_ms(
    tx_chars: u32,
    rx_chars: u32,
    char_half_etu: u32,
    fi: u16,
    di: u8,
    card_clk_hz: u32,
) -> u32 {
    if di == 0 || card_clk_hz == 0 || fi == 0 {
        return u32::MAX;
    }
    let half_etu_total = (tx_chars.saturating_add(rx_chars)) as u64 * char_half_etu as u64;
    let micros = half_etu_total * fi as u64 * 1_000_000 / (2 * di as u64 * card_clk_hz as u64);
    micros.div_ceil(1_000).min(u32::MAX as u64) as u32
}

/// T=0 work waiting time, in milliseconds: WWT = WI × 960 etu
/// (ISO 7816-3 §8.1/§10.2, quoted by osmo-ccid-firmware
/// `ccid_common/iso7816_3.c`: "WT = WI x 960 x (Fi / f(max)) where WI
/// is encoded in TC2 in the ATR (10 if absent) […] after reset WT is
/// 9600 ETU"). WI = 0 is invalid per the reference and is mapped to
/// the ISO default 10 here (the contact T=0 parameter echo carries
/// WI = 0 meaning "default"). Ceiling division like the T=1 helpers.
///
/// #137 documentation duty: the STM32 T=0 procedure timeout is a flat
/// 5000 ms today — that covers the legal WWT for WI ≤ 56 at the 10752
/// default baud (893 ms at the WI=10 default), i.e. the flat value is
/// a conservative over-wait for every bench-realistic card. Deriving
/// it instead (fail faster on mute T=0 cards) is bench-gated on the
/// F469 slot and deliberately not changed without hardware proof.
pub fn t0_wwt_ms(wi: u8, baud: u32) -> u32 {
    if baud == 0 {
        return 5_000;
    }
    let wi = if wi == 0 { 10u64 } else { wi as u64 };
    let wwt_etu = wi * 960;
    let ms = (wwt_etu * 1_000).div_ceil(baud as u64);
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
    fn t1_cwt_ms_matches_iso_7816_3_formula() {
        // CWT = (2^CWI + 11) etu (§11.6.4). CWI=0 → 12 etu ≈ 1.06 ms at
        // the 11290 reference baud → 2 ms ceiling-rounded; CWI=5 (the
        // G-05 synthetic TB3) → 43 etu ≈ 3.8 ms → 4; CWI=13 (the ATR
        // parser default) → 8203 etu ≈ 726.6 ms → 727.
        assert_eq!(t1_cwt_ms(0, 11_290), 2);
        assert_eq!(t1_cwt_ms(5, 11_290), 4);
        assert_eq!(t1_cwt_ms(13, 11_290), 727);
        assert_eq!(t1_cwt_ms(0, 10_752), 2);
        assert_eq!(t1_cwt_ms(13, 0), T1_INTER_BYTE_FLOOR_MS);
    }

    #[test]
    fn t1_inter_byte_policy_floors_at_the_bench_proven_50ms() {
        // Low-CWI cards keep the exact #132-era timing: the derived CWT
        // (a few ms) floors at the historical 50 ms.
        assert_eq!(t1_inter_byte_timeout_ms(0, 11_290), 50);
        assert_eq!(t1_inter_byte_timeout_ms(5, 10_752), 50);
        // High-CWI cards: the reader stays at least as patient as the
        // CWT it advertises — CWI=13 ≈ 727 ms at 11290, CWI=15 at 9600
        // ≈ 3.4 s (still under the 5 s cap).
        assert_eq!(t1_inter_byte_timeout_ms(13, 11_290), 727);
        assert_eq!(t1_inter_byte_timeout_ms(15, 9_600), 3_415);
        // Never less patient than the raw CWT, never above the cap.
        for cwi in 0u8..=15 {
            for baud in [9_600u32, 10_752, 11_290, 38_400, 125_000] {
                let cwt = t1_cwt_ms(cwi, baud);
                let ib = t1_inter_byte_timeout_ms(cwi, baud);
                assert!(ib >= cwt, "cwi={cwi} baud={baud}: {ib} < CWT {cwt}");
                assert!(ib <= T1_FIRST_BYTE_MAX_MS);
                assert!(ib >= T1_INTER_BYTE_FLOOR_MS);
            }
        }
    }

    /// THE #137 INVARIANT, forced explicitly: for every BWI the reader
    /// can advertise or echo (ATR TB3 nibble and SetParameters raw[3]
    /// high nibble are both unchecked 0..=15) and every realistic wire
    /// baud, the first-byte wait covers the advertised BWT — except
    /// where the documented 5 s ceiling saturates it (no time
    /// extension, the issue's NON-GOAL). If this test breaks, the cap
    /// decision moved: re-derive against ISO 7816-3 §11.6.2.1 and
    /// re-make it consciously.
    #[test]
    fn t1_first_byte_timeout_covers_advertised_bwt_or_saturates_explicitly() {
        let bauds = [9_600u32, 10_752, 11_290, 12_903, 38_400, 125_000];
        for bwi in 0u8..=15 {
            for baud in bauds {
                let bwt = t1_bwt_ms(bwi, baud);
                let fb = t1_first_byte_timeout_ms(bwi, baud);
                assert!(fb >= T1_FIRST_BYTE_FLOOR_MS);
                if bwt <= T1_FIRST_BYTE_MAX_MS {
                    assert!(
                        fb >= bwt,
                        "bwi={bwi} baud={baud}: first-byte {fb} < advertised BWT {bwt}"
                    );
                } else {
                    assert_eq!(
                        fb, T1_FIRST_BYTE_MAX_MS,
                        "bwi={bwi} baud={baud}: above-cap BWT must saturate at the ceiling"
                    );
                }
            }
        }
    }

    /// Locks the exact cap cross-over at the bench-relevant bauds so a
    /// formula or ceiling change cannot move the advertised-patience
    /// boundary silently (the #137 close-out audit note: the test must
    /// FORCE the cap decision, not assume it).
    #[test]
    fn t1_first_byte_cap_cross_over_is_pinned() {
        // Cherry default 10752 (4 MHz / 372): BWI=5 → 2859 ms (covered),
        // BWI=6 → 5716 ms (saturates at 5000).
        assert_eq!(t1_bwt_ms(5, 10_752), 2_859);
        assert_eq!(t1_first_byte_timeout_ms(5, 10_752), 2_859);
        assert_eq!(t1_bwt_ms(6, 10_752), 5_716);
        assert_eq!(t1_first_byte_timeout_ms(6, 10_752), 5_000);
        // 11290 reference baud: same shape one BWI later.
        assert_eq!(t1_first_byte_timeout_ms(6, 11_290), 5_000);
        // PPS-speed 125000: BWI=9 (491531 etu ≈ 3932 ms) stays covered;
        // BWI=10 (983051 etu ≈ 7864 ms) saturates.
        assert_eq!(t1_first_byte_timeout_ms(9, 125_000), 3_933);
        assert_eq!(t1_first_byte_timeout_ms(10, 125_000), 5_000);
    }

    #[test]
    fn t0_wwt_ms_matches_osmo_iso_7816_reference() {
        // WWT = WI × 960 etu; WI defaults to 10 (9600 etu ≈ 893 ms at
        // the 10752 default baud). osmo iso7816_3.c: "after reset WT
        // is 9600 ETU"; WI=0 is invalid → mapped to the default.
        assert_eq!(t0_wwt_ms(10, 10_752), 893);
        assert_eq!(t0_wwt_ms(10, 11_290), 851);
        assert_eq!(t0_wwt_ms(1, 11_290), 86);
        assert_eq!(t0_wwt_ms(0, 11_290), t0_wwt_ms(10, 11_290));
        assert_eq!(t0_wwt_ms(0, 0), 5_000);
        // Documented margin vs the flat 5000 ms procedure timeout: the
        // flat value covers the legal WWT up to WI=56 at 10752 (covered
        // exactly at the boundary) and stops covering at WI=57.
        assert_eq!(t0_wwt_ms(56, 10_752), 5_000);
        assert_eq!(t0_wwt_ms(57, 10_752), 5_090);
    }

    #[test]
    fn t1_bwi_cwi_ifsc_and_tb3_round_trip() {
        let raw = [0x11, 0x00, 0x00, 0x45, 0x00, 0xFE, 0x00];
        assert_eq!(t1_bwi_cwi_ifsc(&raw), (4, 5, 254));
        assert_eq!(t1_tb3(4, 5), 0x45);
        assert_eq!(t1_tb3(0, 3), 0x03);
    }

    /// #122 decomposition lock, part 1 — the bench wire dominates the
    /// 42.8 ms p50. The F469 wire: Fi=372/Di=1 (ATR default, PPS
    /// speed-up gated off), card clock 4.2 MHz (pclk1 42 MHz /
    /// prescaler 5). The pcscd 25×SELECT workload's block pair is a
    /// 17-char I-block (3 prologue + 13-char SELECT-by-AID APDU + LRC)
    /// plus a 6-char SW response block (3 + SW + LRC). Under the
    /// pre-#135 framing (12 ETU/char) that is 23 × 12 × 88.6 µs ≈
    /// 24.4 ms of pure wire time per exchange — leaving ~10-15 ms card
    /// think + ~1-2 ms firmware for the measured constant.
    #[test]
    fn t1_wire_time_bench_default_di_dominates_42ms() {
        // p50 42.8 ms (docs/PERF_AND_DRY_STRATEGY.md table, n=300)
        let wire = t1_exchange_wire_time_ms(17, 6, T1_CHAR_HALF_ETU_LEGACY_N2, 372, 1, 4_200_000);
        assert!((24..=25).contains(&wire), "wire={wire}ms");
    }

    /// #122 decomposition lock, part 2 — the lever: the same workload
    /// at a PPS-negotiated higher Di drops the wire term by the Di
    /// ratio, to ~3 ms at Di=8 (TA1=0x14-class; the bench ComSign eID
    /// offers TA1=0x18 → Di=12, faster still). This is the arithmetic
    /// behind "42.8 → ~11 ms needs the wire speed-up, not firmware
    /// micro-optimization" (host-side cost already refuted by the
    /// strace evidence on the issue: ACR 8.8 / CardMan 10.9 on the
    /// same host, same pass).
    #[test]
    fn t1_wire_time_pps_di8_is_one_eighth() {
        let legacy = t1_exchange_wire_time_ms(17, 6, T1_CHAR_HALF_ETU_LEGACY_N2, 372, 1, 4_200_000);
        let pps = t1_exchange_wire_time_ms(17, 6, T1_CHAR_HALF_ETU_LEGACY_N2, 372, 8, 4_200_000);
        assert!((3..=4).contains(&pps), "pps={pps}ms");
        // ceiling rounding keeps the ratio within one quantum of 8×
        let ratio = legacy as f32 / pps as f32;
        assert!((6.0..=9.0).contains(&ratio), "ratio={ratio}");
    }

    /// #122/#135 interaction: the corrected conservative framing
    /// (STOP=1.5 + GT=2, ≤13.5 ETU/char) costs at most ~12.5% more wire
    /// time than the legacy framing — never enough to offset the 8×
    /// PPS lever; correctness does not pay for itself in latency here.
    #[test]
    fn t1_wire_time_post_135_framing_overhead_is_minor() {
        let legacy = t1_exchange_wire_time_ms(17, 6, T1_CHAR_HALF_ETU_LEGACY_N2, 372, 1, 4_200_000);
        let corrected =
            t1_exchange_wire_time_ms(17, 6, T1_CHAR_HALF_ETU_POST_135_MAX, 372, 1, 4_200_000);
        assert!(corrected >= legacy);
        assert!(corrected <= legacy + legacy / 8 + 2);
    }

    /// Degenerate inputs saturate instead of dividing by zero.
    #[test]
    fn t1_wire_time_invalid_inputs_saturate() {
        assert_eq!(
            t1_exchange_wire_time_ms(17, 6, 24, 372, 0, 4_200_000),
            u32::MAX
        );
        assert_eq!(
            t1_exchange_wire_time_ms(17, 6, 24, 0, 1, 4_200_000),
            u32::MAX
        );
        assert_eq!(t1_exchange_wire_time_ms(17, 6, 24, 372, 1, 0), u32::MAX);
    }
}
