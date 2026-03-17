mod ccid_harness;

use ccid_firmware_rs::ccid_core::{
    CCID_HEADER_SIZE, COMMAND_STATUS_FAILED, COMMAND_STATUS_NO_ERROR, ICC_STATUS_NO_ICC,
    ICC_STATUS_PRESENT_ACTIVE, ICC_STATUS_PRESENT_INACTIVE, PC_TO_RDR_ESCAPE,
    PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_POWER_OFF, PC_TO_RDR_ICC_POWER_ON,
    PC_TO_RDR_SET_PARAMETERS, PC_TO_RDR_XFR_BLOCK, RDR_TO_PC_DATABLOCK, RDR_TO_PC_ESCAPE,
    RDR_TO_PC_PARAMETERS, RDR_TO_PC_SLOTSTATUS,
};
use ccid_firmware_rs::mock_driver::MockSmartcardDriver;
use ccid_harness::{parse_ccid_response, CcidTestHarness};

const SEEDKEEPER_ATR: &[u8] = &[
    0x3B, 0xFA, 0x18, 0x00, 0x00, 0x81, 0x31, 0xFE, 0x45, 0x4A, 0x54, 0x61, 0x78, 0x43, 0x6F, 0x72,
    0x65, 0x56, 0x31, 0xB2,
];

struct ReplayCapture {
    name: &'static str,
    tx: &'static [u8],
    rx_msg_type: u8,
    rx_status: u8,
    rx_error: u8,
    rx_payload: &'static [u8],
}

fn captures() -> Vec<ReplayCapture> {
    vec![
        // 0: GET_SLOT_STATUS (init)
        ReplayCapture {
            name: "GET_SLOT_STATUS (init)",
            tx: &[0x65, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
            rx_msg_type: RDR_TO_PC_SLOTSTATUS,
            rx_status: 0x01, // ICC present, inactive
            rx_error: 0x00,
            rx_payload: &[],
        },
        // 1: ICC_POWER_ON (auto voltage) -> ATR
        ReplayCapture {
            name: "ICC_POWER_ON -> ATR",
            tx: &[0x62, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00],
            rx_msg_type: RDR_TO_PC_DATABLOCK,
            rx_status: 0x00, // ICC present active
            rx_error: 0x00,
            rx_payload: SEEDKEEPER_ATR,
        },
        // 2: ICC_POWER_OFF
        ReplayCapture {
            name: "ICC_POWER_OFF",
            tx: &[0x63, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00],
            rx_msg_type: RDR_TO_PC_SLOTSTATUS,
            rx_status: 0x01, // ICC present, inactive (card still inserted)
            rx_error: 0x00,
            rx_payload: &[],
        },
        // 3: ICC_POWER_ON (second, for connect)
        ReplayCapture {
            name: "ICC_POWER_ON (connect)",
            tx: &[0x62, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x01, 0x00, 0x00],
            rx_msg_type: RDR_TO_PC_DATABLOCK,
            rx_status: 0x00, // ICC present active
            rx_error: 0x00,
            rx_payload: SEEDKEEPER_ATR,
        },
        // 4: XFRBLOCK: PPS request (T=1 S-block)
        ReplayCapture {
            name: "XFRBLOCK: PPS (FF 11 18 F6)",
            tx: &[
                0x6F, 0x04, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0xFF, 0x11, 0x18, 0xF6,
            ],
            rx_msg_type: RDR_TO_PC_DATABLOCK,
            rx_status: 0x00, // ICC present active
            rx_error: 0x00,
            rx_payload: &[0xFF, 0x11, 0x18, 0xF6], // PPS confirm
        },
        // 5: SET_PARAMETERS: T=1 protocol
        // Note: The firmware returns ATR-derived protocol params, not an echo
        // of the host's requested params. This is correct per CCID spec.
        // SeedKeeper ATR gives: ta1=0x18, edc=CRC(1), guard=0, bwi=4->3, ifsc=254
        ReplayCapture {
            name: "SETPARAMETERS: T=1, IFSC=254",
            tx: &[
                0x61, 0x07, 0x00, 0x00, 0x00, 0x00, 0x05, 0x01, 0x00, 0x00, 0x18, 0x10, 0x00, 0x45,
                0x00, 0xFE, 0x00,
            ],
            rx_msg_type: RDR_TO_PC_PARAMETERS,
            rx_status: 0x00, // ICC present active
            rx_error: 0x00,
            rx_payload: &[0x18, 0x00, 0x00, 0x03, 0x00, 0xFE, 0x00],
        },
        // 6: XFRBLOCK: IFS negotiation (S-block)
        ReplayCapture {
            name: "XFRBLOCK: IFS S-block (IFSD=254)",
            tx: &[
                0x6F, 0x05, 0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0xC1, 0x01, 0xFE,
                0x3E,
            ],
            rx_msg_type: RDR_TO_PC_DATABLOCK,
            rx_status: 0x00, // ICC present active
            rx_error: 0x00,
            rx_payload: &[0x00, 0xE1, 0x01, 0xFE, 0x1E], // IFS response
        },
        // 7: XFRBLOCK: SELECT AID (T=1 I-block with APDU)
        ReplayCapture {
            name: "XFRBLOCK: SELECT AID (T=1 framed)",
            tx: &[
                0x6F, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0xA0,
                0x00, 0x00, 0x00, 0x62, 0x01, 0x01, 0x00, 0xCA,
            ],
            rx_msg_type: RDR_TO_PC_DATABLOCK,
            rx_status: 0x00, // ICC present active
            rx_error: 0x00,
            rx_payload: &[0x00, 0x00, 0x02, 0x67, 0x00, 0x65], // T=1 I-block, SW=6700
        },
        // 8: XFRBLOCK: VERIFY PIN (T=1 I-block with APDU)
        ReplayCapture {
            name: "XFRBLOCK: VERIFY PIN (T=1 framed)",
            tx: &[
                0x6F, 0x0E, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x40, 0x0A, 0x00,
                0x20, 0x00, 0x81, 0x04, 0x31, 0x32, 0x33, 0x34, 0xFF, 0x14,
            ],
            rx_msg_type: RDR_TO_PC_DATABLOCK,
            rx_status: 0x00, // ICC present active
            rx_error: 0x00,
            rx_payload: &[0x00, 0x40, 0x02, 0x6E, 0x00, 0x2C], // T=1 I-block, SW=6E00
        },
        // 9: ICC_POWER_OFF (disconnect)
        ReplayCapture {
            name: "ICC_POWER_OFF (disconnect)",
            tx: &[0x63, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0x00, 0x00, 0x00],
            rx_msg_type: RDR_TO_PC_SLOTSTATUS,
            rx_status: 0x01, // ICC present, inactive
            rx_error: 0x00,
            rx_payload: &[],
        },
    ]
}

fn build_mock_driver() -> MockSmartcardDriver {
    MockSmartcardDriver::new()
        .card_present(true)
        .with_atr(SEEDKEEPER_ATR)
        .with_protocol(0x01)
        .with_apdu_response(&[0x00, 0x00, 0x02, 0x67, 0x00, 0x65])
        .with_apdu_response(&[0x00, 0x40, 0x02, 0x6E, 0x00, 0x2C])
}

#[test]
fn replay_seedkeeper_full_session() {
    let driver = build_mock_driver();
    let mut h = CcidTestHarness::new(driver, 0x08E6);
    let captures = captures();

    for (i, cap) in captures.iter().enumerate() {
        let resp_bytes = h.send_raw(cap.tx);
        let r = parse_ccid_response(&resp_bytes);

        assert_eq!(
            r.msg_type, cap.rx_msg_type,
            "[{}] step {}: expected msg_type 0x{:02X}, got 0x{:02X}",
            cap.name, i, cap.rx_msg_type, r.msg_type
        );

        let icc_status = r.b_status & 0x03;
        let cmd_status = (r.b_status >> 6) & 0x03;
        let expected_icc_status = cap.rx_status & 0x03;
        let expected_cmd_status = (cap.rx_status >> 6) & 0x03;

        assert_eq!(
            cmd_status, expected_cmd_status,
            "[{}] step {}: cmd_status expected 0x{:02X}, got 0x{:02X}",
            cap.name, i, expected_cmd_status, cmd_status
        );

        assert_eq!(
            icc_status, expected_icc_status,
            "[{}] step {}: icc_status expected 0x{:02X}, got 0x{:02X}",
            cap.name, i, expected_icc_status, icc_status
        );

        assert_eq!(
            r.b_error, cap.rx_error,
            "[{}] step {}: b_error expected 0x{:02X}, got 0x{:02X}",
            cap.name, i, cap.rx_error, r.b_error
        );

        assert_eq!(
            r.data, cap.rx_payload,
            "[{}] step {}: payload mismatch",
            cap.name, i
        );
    }
}

// Individual replay tests for debugging

#[test]
fn replay_get_slot_status_no_card() {
    let driver = MockSmartcardDriver::new().card_present(false);
    let mut h = CcidTestHarness::new(driver, 0x08E6);
    let resp = h.send(PC_TO_RDR_GET_SLOT_STATUS, &[], 0x01);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_SLOTSTATUS);
    assert_eq!(r.icc_status(), ICC_STATUS_NO_ICC);
    assert!(r.is_success());
}

#[test]
fn replay_get_slot_status_card_present() {
    let driver = MockSmartcardDriver::new().card_present(true);
    let mut h = CcidTestHarness::new(driver, 0x08E6);
    let resp = h.send(PC_TO_RDR_GET_SLOT_STATUS, &[], 0x01);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_SLOTSTATUS);
    assert_eq!(r.icc_status(), ICC_STATUS_PRESENT_INACTIVE);
    assert!(r.is_success());
}

#[test]
fn replay_power_on_seedkeeper_atr() {
    let driver = MockSmartcardDriver::new()
        .card_present(true)
        .with_atr(SEEDKEEPER_ATR);
    let mut h = CcidTestHarness::new(driver, 0x08E6);
    let resp = h.send(PC_TO_RDR_ICC_POWER_ON, &[], 0x01);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_DATABLOCK);
    assert!(r.is_success());
    assert_eq!(r.icc_status(), ICC_STATUS_PRESENT_ACTIVE);
    assert_eq!(r.data, SEEDKEEPER_ATR);
}

#[test]
fn replay_power_on_power_off_cycle() {
    let driver = MockSmartcardDriver::new()
        .card_present(true)
        .with_atr(SEEDKEEPER_ATR);
    let mut h = CcidTestHarness::new(driver, 0x08E6);

    // Power on
    let resp = h.send(PC_TO_RDR_ICC_POWER_ON, &[], 0x01);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_DATABLOCK);
    assert_eq!(r.icc_status(), ICC_STATUS_PRESENT_ACTIVE);

    // Slot status should show active
    let resp = h.send(PC_TO_RDR_GET_SLOT_STATUS, &[], 0x02);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.icc_status(), ICC_STATUS_PRESENT_ACTIVE);

    // Power off
    let resp = h.send(PC_TO_RDR_ICC_POWER_OFF, &[], 0x03);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_SLOTSTATUS);
    assert_eq!(r.icc_status(), ICC_STATUS_PRESENT_INACTIVE);
}

#[test]
fn replay_escape_get_firmware_features() {
    let driver = MockSmartcardDriver::new().card_present(true);
    let mut h = CcidTestHarness::new(driver, 0x08E6);
    // The firmware returns CMD_NOT_SUPPORTED for unrecognized escape subcommands
    let mut msg = vec![0u8; CCID_HEADER_SIZE + 1];
    msg[0] = PC_TO_RDR_ESCAPE;
    msg[6] = 0x01; // seq
    msg[CCID_HEADER_SIZE] = 0x6A; // GET_FIRMWARE_FEATURES
    let resp = h.send_raw(&msg);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_ESCAPE);
    assert!(r.is_cmd_failed());
}

#[test]
fn replay_set_parameters_t1() {
    let driver = MockSmartcardDriver::new()
        .card_present(true)
        .with_atr(SEEDKEEPER_ATR);
    let mut h = CcidTestHarness::new(driver, 0x08E6);

    // Power on first
    h.send(PC_TO_RDR_ICC_POWER_ON, &[], 0x01);

    // Set parameters: T=1, IFSC=254, BWI=1, CWI=4
    let params = &[0x18, 0x10, 0x00, 0x45, 0x00, 0xFE, 0x00];
    let resp = h.send(PC_TO_RDR_SET_PARAMETERS, params, 0x02);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_PARAMETERS);
    assert!(r.is_success());
    // Firmware returns ATR-derived params, not echo of request
    assert_eq!(r.data[0], 0x18); // ta1
    assert_eq!(r.data[5], 0xFE); // ifsc=254
}

#[test]
fn replay_xfrblock_pps() {
    let driver = MockSmartcardDriver::new()
        .card_present(true)
        .with_atr(SEEDKEEPER_ATR)
        .with_protocol(0x01)
        .with_apdu_response(&[0xFF, 0x11, 0x18, 0xF6]);
    let mut h = CcidTestHarness::new(driver, 0x08E6);

    // Power on first
    h.send(PC_TO_RDR_ICC_POWER_ON, &[], 0x01);

    // PPS via XfrBlock
    let pps_req = &[0xFF, 0x11, 0x18, 0xF6];
    let resp = h.send(PC_TO_RDR_XFR_BLOCK, pps_req, 0x02);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_DATABLOCK);
    assert!(r.is_success());
    assert_eq!(r.data, pps_req);
}

#[test]
fn replay_xfrblock_select_aid() {
    let driver = MockSmartcardDriver::new()
        .card_present(true)
        .with_atr(SEEDKEEPER_ATR)
        .with_protocol(0x01)
        .with_apdu_response(&[0x00, 0x00, 0x02, 0x67, 0x00, 0x65]);
    let mut h = CcidTestHarness::new(driver, 0x08E6);

    // Power on
    h.send(PC_TO_RDR_ICC_POWER_ON, &[], 0x01);

    // SELECT AID (T=1 framed I-block)
    let select_t1 = &[
        0x00, 0x00, 0x08, 0xA0, 0x00, 0x00, 0x00, 0x62, 0x01, 0x01, 0x00, 0xCA,
    ];
    let resp = h.send(PC_TO_RDR_XFR_BLOCK, select_t1, 0x02);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_DATABLOCK);
    assert!(r.is_success());
    assert_eq!(r.data, &[0x00, 0x00, 0x02, 0x67, 0x00, 0x65]);
    // SW bytes 6700 are visible in the response
}

#[test]
fn replay_xfrblock_verify_pin() {
    let driver = MockSmartcardDriver::new()
        .card_present(true)
        .with_atr(SEEDKEEPER_ATR)
        .with_protocol(0x01)
        .with_apdu_response(&[0x00, 0x40, 0x02, 0x6E, 0x00, 0x2C]);
    let mut h = CcidTestHarness::new(driver, 0x08E6);

    // Power on
    h.send(PC_TO_RDR_ICC_POWER_ON, &[], 0x01);

    // VERIFY PIN (T=1 framed I-block)
    let verify_t1 = &[
        0x00, 0x40, 0x0A, 0x00, 0x20, 0x00, 0x81, 0x04, 0x31, 0x32, 0x33, 0x34, 0xFF, 0x14,
    ];
    let resp = h.send(PC_TO_RDR_XFR_BLOCK, verify_t1, 0x02);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_DATABLOCK);
    assert!(r.is_success());
    assert_eq!(r.data, &[0x00, 0x40, 0x02, 0x6E, 0x00, 0x2C]);
    // SW bytes 6E00 are visible in the response
}

#[test]
fn replay_full_session_with_verify() {
    let driver = MockSmartcardDriver::new()
        .card_present(true)
        .with_atr(SEEDKEEPER_ATR)
        .with_protocol(0x01)
        .with_apdu_response(&[0x00, 0x00, 0x02, 0x67, 0x00, 0x65]) // SELECT response
        .with_apdu_response(&[0x00, 0x40, 0x02, 0x6E, 0x00, 0x2C]); // VERIFY response
    let mut h = CcidTestHarness::new(driver, 0x08E6);

    // 1. Slot status (card present, inactive)
    let resp = h.send(PC_TO_RDR_GET_SLOT_STATUS, &[], 0x01);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.icc_status(), ICC_STATUS_PRESENT_INACTIVE);

    // 2. Power on -> get ATR
    let resp = h.send(PC_TO_RDR_ICC_POWER_ON, &[], 0x02);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_DATABLOCK);
    assert_eq!(r.data, SEEDKEEPER_ATR);

    // 3. Slot status (now active)
    let resp = h.send(PC_TO_RDR_GET_SLOT_STATUS, &[], 0x03);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.icc_status(), ICC_STATUS_PRESENT_ACTIVE);

    // 4. PPS
    let resp = h.send(PC_TO_RDR_XFR_BLOCK, &[0xFF, 0x11, 0x18, 0xF6], 0x04);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.data, &[0xFF, 0x11, 0x18, 0xF6]);

    // 5. Set parameters
    let resp = h.send(
        PC_TO_RDR_SET_PARAMETERS,
        &[0x18, 0x10, 0x00, 0x45, 0x00, 0xFE, 0x00],
        0x05,
    );
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_PARAMETERS);

    // 6. IFS negotiation
    let resp = h.send(PC_TO_RDR_XFR_BLOCK, &[0x00, 0xC1, 0x01, 0xFE, 0x3E], 0x06);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_DATABLOCK);
    assert_eq!(r.data, &[0x00, 0xE1, 0x01, 0xFE, 0x1E]);

    // 7. SELECT AID
    let resp = h.send(
        PC_TO_RDR_XFR_BLOCK,
        &[
            0x00, 0x00, 0x08, 0xA0, 0x00, 0x00, 0x00, 0x62, 0x01, 0x01, 0x00, 0xCA,
        ],
        0x07,
    );
    let r = parse_ccid_response(&resp);
    assert_eq!(r.data, &[0x00, 0x00, 0x02, 0x67, 0x00, 0x65]);

    // 8. VERIFY PIN
    let resp = h.send(
        PC_TO_RDR_XFR_BLOCK,
        &[
            0x00, 0x40, 0x0A, 0x00, 0x20, 0x00, 0x81, 0x04, 0x31, 0x32, 0x33, 0x34, 0xFF, 0x14,
        ],
        0x08,
    );
    let r = parse_ccid_response(&resp);
    assert_eq!(r.data, &[0x00, 0x40, 0x02, 0x6E, 0x00, 0x2C]);

    // 9. Power off
    let resp = h.send(PC_TO_RDR_ICC_POWER_OFF, &[], 0x09);
    let r = parse_ccid_response(&resp);
    assert_eq!(r.msg_type, RDR_TO_PC_SLOTSTATUS);
    assert_eq!(r.icc_status(), ICC_STATUS_PRESENT_INACTIVE);
}
