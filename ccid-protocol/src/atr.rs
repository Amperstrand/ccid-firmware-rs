use core::default::Default;

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
        0, 372, 558, 744, 1116, 1488, 1860, 0, 0, 512, 768, 1024, 1536, 2048, 0, 0,
    ];
    FI_TABLE.get(nibble as usize).copied().unwrap_or(372)
}

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
            level += 1;
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
}
