pub use ccid_protocol::atr::*;
pub use ccid_protocol::status::build_bstatus;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fi_mapping_values() {
        // Inspired by osmo-ccid-firmware/ccid_common/iso7816_3.c::iso7816_3_fi_table
        assert_eq!(fi_from_ta1_high(1), 372);
        assert_eq!(fi_from_ta1_high(2), 558);
        assert_eq!(fi_from_ta1_high(3), 744);
        assert_eq!(fi_from_ta1_high(9), 512);
        assert_eq!(fi_from_ta1_high(13), 2048);
    }

    #[test]
    fn test_di_mapping_values() {
        // Inspired by osmo-ccid-firmware/ccid_common/iso7816_3.c::iso7816_3_di_table
        assert_eq!(di_from_ta1_low(1), 1);
        assert_eq!(di_from_ta1_low(2), 2);
        assert_eq!(di_from_ta1_low(3), 4);
        assert_eq!(di_from_ta1_low(8), 12);
        assert_eq!(di_from_ta1_low(9), 20);
    }

    #[test]
    fn test_parse_atr_t1_protocol() {
        // Inspired by osmo-ccid-firmware/ccid_common/iso7816_fsm.c::atr_fsm_wait_td
        let atr = [0x3B, 0x80, 0x01];
        let p = parse_atr(&atr);
        assert_eq!(p.protocol, 1);
    }

    #[test]
    fn test_t0_procedure_classification() {
        // Inspired by osmo-ccid-firmware/ccid_common/iso7816_fsm.c::tpdu_s_procedure_action
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
    fn test_activation_order_matches_reference() {
        // Inspired by osmo-ccid-firmware/ccid_host/cuart_fsm_test.c::main
        // sequence: RST active -> POWER on -> CLOCK on -> RST release.
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
    fn test_ccid_status_byte_packing() {
        // Inspired by osmo-ccid-firmware/ccid_common/ccid_device.c::SET_HDR_IN usage.
        assert_eq!(build_bstatus(0, 0), 0x00);
        assert_eq!(build_bstatus(1, 0), 0x40);
        assert_eq!(build_bstatus(0, 2), 0x02);
        assert_eq!(build_bstatus(1, 2), 0x42);
    }

    #[test]
    fn test_tck_verification_t1_valid() {
        // ATR: TS=0x3B, T0=0x80 (TD1 present), TD1=0x01 (T=1, no more), TCK=0x81
        // TCK = T0 XOR TD1 = 0x80 XOR 0x01 = 0x81 ✓
        let atr = [0x3B, 0x80, 0x01, 0x81];
        let p = parse_atr(&atr);
        assert_eq!(p.protocol, 1);
        assert!(verify_atr_tck(&atr, p.protocol));
    }

    #[test]
    fn test_tck_verification_t1_invalid() {
        // Same ATR but corrupted TCK
        let atr = [0x3B, 0x80, 0x01, 0xFF];
        let p = parse_atr(&atr);
        assert_eq!(p.protocol, 1);
        assert!(!verify_atr_tck(&atr, p.protocol));
    }

    #[test]
    fn test_tck_verification_t0_skipped() {
        // T=0 ATR — TCK verification should be skipped
        let atr = [0x3B, 0x00, 0xFF];
        let p = parse_atr(&atr);
        assert_eq!(p.protocol, 0);
        assert!(verify_atr_tck(&atr, p.protocol));
    }

    #[test]
    fn test_tck_verification_short_atr() {
        // ATR too short for TCK
        let atr = [0x3B, 0x01];
        assert!(verify_atr_tck(&atr, 1));
    }
}
