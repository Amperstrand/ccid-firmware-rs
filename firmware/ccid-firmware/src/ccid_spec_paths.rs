//! Host-side CCID spec-path characterization tests (issue #53).
//!
//! Exercises `CcidMessageHandler` directly over mock/flippable smartcard
//! drivers — the same handler the STM32 USB transport (`ccid.rs`) drives.
//! Covers the previously untested spec paths:
//!
//! - Abort / AbortSlot sequence (firmware stub — behavior pinned honestly)
//! - bSeq handling (verbatim echo, no monotonicity validation)
//! - BWI time-extension surface (T=1 BWI/CWI via parameters; XfrBlock bBWI)
//! - T=0 APDU command handling and Secure (PIN pad) acceptance per vendor
//! - Interrupt-IN NotifySlotChange encoding (STM32 path)
//! - Mid-transfer card removal behavior
//!
//! Not covered here (host layer cannot reach it): the T=1 WTX negotiation
//! lives in `t1_engine.rs`, which is `cfg(all(target_arch = "arm",
//! target_os = "none"))` and uses defmt/cortex-m; the dwFeatures/bPINSupport
//! descriptor values live in `device_profile.rs`, also ARM-only (its inline
//! tests do not execute in the host CI job). Vendor-id-based profile
//! behavior (PIN pad only on Cherry 046A) is covered at handler level.
//!
//! Quotes marked `CCID_SPEC:` are verified verbatim against
//! `reference/osmo-ccid-firmware/ccid_common/ccid_proto.h` by the
//! spec-quote-drift CI job; `CCID_SERIAL:` quotes against
//! `reference/CCID/src/ccid_serial.c`.

use crate::ccid_core::CcidMessageHandler;
use crate::driver::SmartcardDriver;
use crate::mock_driver::{MockCall, MockError, MockSmartcardDriver};
use crate::pinpad::PinResult;
use ccid_protocol::types::{
    SlotState, CCID_ERR_CMD_NOT_SUPPORTED, CCID_ERR_CMD_SLOT_BUSY, CCID_HEADER_SIZE,
    COMMAND_STATUS_FAILED, COMMAND_STATUS_NO_ERROR, COMMAND_STATUS_TIME_EXTENSION,
    ICC_STATUS_NO_ICC, ICC_STATUS_PRESENT_ACTIVE, ICC_STATUS_PRESENT_INACTIVE, PC_TO_RDR_ABORT,
    PC_TO_RDR_GET_PARAMETERS, PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_POWER_ON,
    PC_TO_RDR_RESET_PARAMETERS, PC_TO_RDR_SECURE, PC_TO_RDR_SET_PARAMETERS, PC_TO_RDR_T0_APDU,
    PC_TO_RDR_XFR_BLOCK, RDR_TO_PC_DATABLOCK, RDR_TO_PC_NOTIFY_SLOT_CHANGE, RDR_TO_PC_PARAMETERS,
    RDR_TO_PC_SLOTSTATUS,
};

// Cherry SmartTerminal ST-2xxx vendor id — the only profile with a PIN pad.
const CHERRY_VID: u16 = 0x046A;
// Gemalto IDBridge CT30 / K30 vendor id — profiles without PIN pad.
const GEMALTO_VID: u16 = 0x08E6;

/// Build a PC_to_RDR bulk-OUT message: 10-byte CCID header + payload.
/// `specific` occupies header bytes 7..9 (bBWI/bProtocolNum/bPowerSelect/...).
fn ccid_request(msg_type: u8, slot: u8, seq: u8, specific: [u8; 3], payload: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(CCID_HEADER_SIZE + payload.len());
    msg.push(msg_type);
    msg.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    msg.push(slot);
    msg.push(seq);
    msg.extend_from_slice(&specific);
    msg.extend_from_slice(payload);
    msg
}

/// Feed one message, handle it, and take the response.
fn exchange<D: SmartcardDriver>(h: &mut CcidMessageHandler<D>, msg: &[u8]) -> Vec<u8> {
    h.set_rx_data(msg);
    assert!(h.message_ready(), "message should be complete");
    h.handle_message();
    let (len, resp) = h.take_response();
    resp[..len].to_vec()
}

fn rseq(resp: &[u8]) -> u8 {
    resp[6]
}
fn cmd_status(resp: &[u8]) -> u8 {
    (resp[7] >> 6) & 0x03
}
fn icc_status(resp: &[u8]) -> u8 {
    resp[7] & 0x03
}
fn berror(resp: &[u8]) -> u8 {
    resp[8]
}
fn param_data(resp: &[u8]) -> &[u8] {
    &resp[CCID_HEADER_SIZE..]
}

/// A T=1 ATR whose TB2 interface byte (0x37) selects BWI=3 / CWI=7.
/// TS=0x3B, T0=0x80 (TD1 present), TD1=0x21 (TB2 present, T=1), TB2=0x37.
const T1_ATR_BWI3_CWI7: [u8; 4] = [0x3B, 0x80, 0x21, 0x37];
/// A minimal T=0 ATR (T0=0x00: no interface bytes, no historical bytes).
const T0_ATR: [u8; 2] = [0x3B, 0x00];

/// Smartcard driver whose card-presence flag can be flipped at runtime, to
/// simulate card removal/insertion between handler calls.
struct FlippableDriver {
    present: bool,
    atr: [u8; 33],
    atr_len: usize,
    power_off_count: u32,
    last_protocol: Option<u8>,
}

impl FlippableDriver {
    fn new(present: bool, atr: &[u8]) -> Self {
        let mut a = [0u8; 33];
        let len = atr.len().min(33);
        a[..len].copy_from_slice(&atr[..len]);
        Self {
            present,
            atr: a,
            atr_len: len,
            power_off_count: 0,
            last_protocol: None,
        }
    }

    fn flip(&mut self, present: bool) {
        self.present = present;
    }
}

impl SmartcardDriver for FlippableDriver {
    type Error = MockError;

    fn power_on(&mut self) -> core::result::Result<&[u8], Self::Error> {
        Ok(&self.atr[..self.atr_len])
    }
    fn power_off(&mut self) {
        self.power_off_count += 1;
    }
    fn is_card_present(&self) -> bool {
        self.present
    }
    fn transmit_apdu(
        &mut self,
        _command: &[u8],
        _response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        Ok(0)
    }
    fn transmit_raw(
        &mut self,
        _data: &[u8],
        _response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        Ok(0)
    }
    fn set_protocol(&mut self, protocol: u8) {
        self.last_protocol = Some(protocol);
    }
    fn set_clock(&mut self, _enable: bool) {}
    fn set_clock_and_rate(
        &mut self,
        clock_hz: u32,
        rate_bps: u32,
    ) -> core::result::Result<(u32, u32), Self::Error> {
        Ok((clock_hz, rate_bps))
    }
}

// ---------------------------------------------------------------------------
// 1. Abort / AbortSlot sequence (documented stub — actual behavior pinned)
// ---------------------------------------------------------------------------

// CCID_SPEC: /* Section 6.1.13 */ struct ccid_pc_to_rdr_abort {
// struct ccid_header hdr; uint8_t abRFU[3]; } __attribute__ ((packed));
// /* Response: RDR_to_PC_SlotStatus */
#[test]
fn test_abort_stub_responds_slot_status_success_for_any_seq() {
    // Given: an idle single-slot reader (no card, no in-flight command)
    let mut h = CcidMessageHandler::new(MockSmartcardDriver::new(), CHERRY_VID);

    // When: the host sends the bulk-OUT leg of the abort sequence
    let resp = exchange(&mut h, &ccid_request(PC_TO_RDR_ABORT, 0, 0x7A, [0; 3], &[]));
    // Then: the stub answers RDR_to_PC_SlotStatus with cmd=OK — nothing is
    // ever aborted because commands run synchronously to completion before
    // the next bulk-OUT message is read.
    assert_eq!(resp[0], RDR_TO_PC_SLOTSTATUS);
    assert_eq!(rseq(&resp), 0x7A);
    assert_eq!(cmd_status(&resp), COMMAND_STATUS_NO_ERROR);
    assert_eq!(berror(&resp), 0);

    // And: a second Abort with an unrelated bSeq also succeeds — the stub
    // does not pair bSeq with a prior control-pipe ABORT request.
    let resp2 = exchange(&mut h, &ccid_request(PC_TO_RDR_ABORT, 0, 0x2B, [0; 3], &[]));
    assert_eq!(resp2[0], RDR_TO_PC_SLOTSTATUS);
    assert_eq!(rseq(&resp2), 0x2B);
    assert_eq!(cmd_status(&resp2), COMMAND_STATUS_NO_ERROR);
}

// CCID_SPEC: enum ccid_class_spec_req { CLASS_SPEC_CCID_ABORT = 0x01,
// CLASS_SPEC_CCID_GET_CLOCK_FREQ = 0x02, CLASS_SPEC_CCID_GET_DATA_RATES = 0x03 };

#[test]
fn test_abort_while_command_in_flight_returns_slot_busy() {
    // Given: a handled command whose response has not been taken yet
    // (cmd_busy set — mirrors the USB window before the IN endpoint drains)
    let mut h = CcidMessageHandler::new(MockSmartcardDriver::new(), CHERRY_VID);
    h.set_rx_data(&ccid_request(
        PC_TO_RDR_GET_SLOT_STATUS,
        0,
        0x10,
        [0; 3],
        &[],
    ));
    h.handle_message();
    assert!(h.cmd_busy());

    // When: Abort for that same bSeq arrives before the response is drained
    h.set_rx_data(&ccid_request(PC_TO_RDR_ABORT, 0, 0x10, [0; 3], &[]));
    h.handle_message();

    // Then: the abort itself is refused with CMD_SLOT_BUSY — the firmware
    // cannot abort an in-flight command (honest characterization of the
    // documented stub, which the class-specific ABORT control request
    // layer above also does not act on).
    let (len, resp) = h.take_response();
    assert_eq!(len, CCID_HEADER_SIZE);
    assert_eq!(resp[0], RDR_TO_PC_SLOTSTATUS);
    assert_eq!(rseq(resp), 0x10);
    assert_eq!(cmd_status(resp), COMMAND_STATUS_FAILED);
    assert_eq!(berror(resp), CCID_ERR_CMD_SLOT_BUSY);
}

// ---------------------------------------------------------------------------
// 2. bSeq sequence-number handling
// ---------------------------------------------------------------------------

// CCID_SPEC: struct ccid_header { uint8_t bMessageType; uint32_t dwLength;
// uint8_t bSlot; uint8_t bSeq; } __attribute__ ((packed));

#[test]
fn test_response_echoes_bseq_verbatim_without_validation() {
    // Given: an idle reader
    let mut h = CcidMessageHandler::new(MockSmartcardDriver::new(), CHERRY_VID);

    // When/Then: GetSlotStatus responses echo the command's bSeq exactly —
    // the firmware performs no monotonicity or out-of-order validation; the
    // sequence numbering contract belongs to the host (bSeq must match
    // between command and response, which every response here satisfies).
    for seq in [0x00u8, 0x01, 0x42, 0x7F, 0xFE, 0xFF] {
        let resp = exchange(
            &mut h,
            &ccid_request(PC_TO_RDR_GET_SLOT_STATUS, 0, seq, [0; 3], &[]),
        );
        assert_eq!(resp[0], RDR_TO_PC_SLOTSTATUS, "seq={:#04x}", seq);
        assert_eq!(rseq(&resp), seq, "seq={:#04x}", seq);
    }
}

#[test]
fn test_bseq_wraparound_255_to_0_is_accepted() {
    // Given: an idle reader that has already answered bSeq=0xFF
    let mut h = CcidMessageHandler::new(MockSmartcardDriver::new(), CHERRY_VID);
    let resp = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_GET_SLOT_STATUS, 0, 0xFF, [0; 3], &[]),
    );
    assert_eq!(rseq(&resp), 0xFF);

    // When: the host wraps the counter to 0x00 (typical pcscd rollover)
    let resp2 = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_GET_SLOT_STATUS, 0, 0x00, [0; 3], &[]),
    );
    // Then: the response echoes 0x00 — no stale-sequence rejection.
    assert_eq!(rseq(&resp2), 0x00);
    assert_eq!(cmd_status(&resp2), COMMAND_STATUS_NO_ERROR);
}

#[test]
fn test_bseq_echoed_on_bad_slot_error() {
    // Given: an idle reader
    let mut h = CcidMessageHandler::new(MockSmartcardDriver::new(), CHERRY_VID);

    // When: a message targets slot 1 on this single-slot reader
    let resp = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_GET_SLOT_STATUS, 1, 0xAB, [0; 3], &[]),
    );
    // Then: the error response still echoes bSeq=0xAB. bError is 0x05
    // (the CCID 1.1 Table 6.2-2 "bad slot" value) — deliberately not
    // carried by a spec-quote marker: ccid_proto.h has no entry for it
    // (quote gap, no fabricated quote).
    assert_eq!(resp[0], RDR_TO_PC_SLOTSTATUS);
    assert_eq!(rseq(&resp), 0xAB);
    assert_eq!(cmd_status(&resp), COMMAND_STATUS_FAILED);
    assert_eq!(berror(&resp), 0x05);
    assert_eq!(icc_status(&resp), ICC_STATUS_NO_ICC);
}

// ---------------------------------------------------------------------------
// 3. BWI time-extension surface (T=1 Block Waiting Time via parameters)
// ---------------------------------------------------------------------------

// CCID_SPEC: struct ccid_proto_data_t1 { uint8_t bmFindexDindex;
// uint8_t bmTCCKST1; uint8_t bGuardTimeT1; uint8_t bWaitingIntegersT1;
// uint8_t bClockStop; uint8_t bIFSC; uint8_t bNadValue; }
// __attribute__ ((packed));

#[test]
fn test_t1_bwi_cwi_from_atr_surfaced_in_get_parameters() {
    // Given: a present T=1 card whose ATR negotiates BWI=3 / CWI=7 (TB2=0x37)
    let driver = MockSmartcardDriver::new()
        .card_present(true)
        .with_atr(&T1_ATR_BWI3_CWI7);
    let mut h = CcidMessageHandler::new(driver, CHERRY_VID);

    // When: the card is powered on and parameters are queried
    let on = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_ICC_POWER_ON, 0, 1, [0x00, 0, 0], &[]),
    );
    assert_eq!(on[0], RDR_TO_PC_DATABLOCK);
    assert_eq!(cmd_status(&on), COMMAND_STATUS_NO_ERROR);
    let gp = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_GET_PARAMETERS, 0, 2, [0; 3], &[]),
    );

    // Then: RDR_to_PC_Parameters reports T=1 with the 7-byte T=1 structure,
    // and bWaitingIntegersT1 carries (BWI<<4)|CWI = 0x37 from the ATR.
    assert_eq!(gp[0], RDR_TO_PC_PARAMETERS);
    assert_eq!(gp[9], 1, "bProtocolNum");
    let params = param_data(&gp);
    assert_eq!(params.len(), 7);
    assert_eq!(params[3], 0x37, "bWaitingIntegersT1 = (BWI<<4)|CWI");
    assert_eq!(params[5], 32, "bIFSC default (no TA3 in ATR)");
}

// CCID_SPEC: struct ccid_pc_to_rdr_set_parameters { struct ccid_header hdr;
// uint8_t bProtocolNum; uint8_t abRFU[2]; union { struct ccid_proto_data_t0 t0;
// struct ccid_proto_data_t1 t1; } abProtocolData; } __attribute__ ((packed));

#[test]
fn test_set_parameters_applies_and_echoes_host_t1_values() {
    // Spec-conformant behavior (issue #56 fixed): SetParameters applies the
    // host-supplied parameter bytes and the RDR_to_PC_Parameters response
    // echoes them back verbatim — the mechanism by which pcscd applies
    // negotiated Fi/Di and BWI/CWI after PPS.
    // Given: an active T=1 card with ATR BWI/CWI = 3/7
    let mut h = CcidMessageHandler::new(
        MockSmartcardDriver::new()
            .card_present(true)
            .with_atr(&T1_ATR_BWI3_CWI7),
        CHERRY_VID,
    );
    exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_ICC_POWER_ON, 0, 1, [0x00, 0, 0], &[]),
    );

    // When: the host sets T=1 parameters requesting BWI/CWI=0x55, IFSC=0xFE
    let host_params: [u8; 7] = [0x11, 0x00, 0x00, 0x55, 0x00, 0xFE, 0x00];
    let resp = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_SET_PARAMETERS, 0, 3, [1, 0, 0], &host_params),
    );

    // Then: success is reported, the driver is told the protocol...
    assert_eq!(resp[0], RDR_TO_PC_PARAMETERS);
    assert_eq!(cmd_status(&resp), COMMAND_STATUS_NO_ERROR);
    assert!(h
        .driver()
        .call_log()
        .iter()
        .any(|c| matches!(c, MockCall::SetProtocol { protocol: 1 })));

    // ...and the response echoes the exact structure the host sent, not the
    // ATR-derived defaults (BWI/CWI 0x37, IFSC 32).
    let params = param_data(&resp);
    assert_eq!(params.len(), 7);
    assert_eq!(params, &host_params);
    assert_eq!(params[3], 0x55, "host-requested BWI/CWI applied");
    assert_eq!(params[5], 0xFE, "host-requested IFSC applied");

    // And: GetParameters afterwards keeps returning the stored host set.
    let gp = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_GET_PARAMETERS, 0, 4, [0; 3], &[]),
    );
    assert_eq!(param_data(&gp), &host_params);
}

#[test]
fn test_set_parameters_rejects_invalid_t1_values_with_bad_parameter() {
    // CCID 1.1 requires invalid SetParameters values to be answered with
    // the bad-parameter error. bError 0x08 ("bad parameter") is asserted
    // without a spec-quote marker: ccid_proto.h has no entry for it
    // (quote gap, no fabricated quote).
    // Given: an active T=1 card
    let mut h = CcidMessageHandler::new(
        MockSmartcardDriver::new()
            .card_present(true)
            .with_atr(&T1_ATR_BWI3_CWI7),
        CHERRY_VID,
    );
    exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_ICC_POWER_ON, 0, 1, [0x00, 0, 0], &[]),
    );

    // When: bIFSC = 0x05 (below the 0x10..=0xFE range)
    let resp = exchange(
        &mut h,
        &ccid_request(
            PC_TO_RDR_SET_PARAMETERS,
            0,
            3,
            [1, 0, 0],
            &[0x11, 0x00, 0x00, 0x55, 0x00, 0x05, 0x00],
        ),
    );
    // Then: failed with the bad-parameter error, no parameter change.
    assert_eq!(resp[0], RDR_TO_PC_PARAMETERS);
    assert_eq!(cmd_status(&resp), COMMAND_STATUS_FAILED);
    assert_eq!(berror(&resp), 0x08);

    // When: bmTCCKST1 = 0x10 (RFU bits set; only the EDC bit is meaningful)
    let resp = exchange(
        &mut h,
        &ccid_request(
            PC_TO_RDR_SET_PARAMETERS,
            0,
            4,
            [1, 0, 0],
            &[0x11, 0x10, 0x00, 0x55, 0x00, 0xFE, 0x00],
        ),
    );
    // Then: rejected the same way.
    assert_eq!(cmd_status(&resp), COMMAND_STATUS_FAILED);
    assert_eq!(berror(&resp), 0x08);

    // When: bNadValue != 0 (NAD is not supported; the field shall be 0)
    let resp = exchange(
        &mut h,
        &ccid_request(
            PC_TO_RDR_SET_PARAMETERS,
            0,
            5,
            [1, 0, 0],
            &[0x11, 0x00, 0x00, 0x55, 0x00, 0xFE, 0x01],
        ),
    );
    assert_eq!(cmd_status(&resp), COMMAND_STATUS_FAILED);
    assert_eq!(berror(&resp), 0x08);
}

// CCID_SPEC: struct ccid_proto_data_t0 { uint8_t bmFindexDindex;
// uint8_t bmTCCKST0; uint8_t bGuardTimeT0; uint8_t bWaitingIntegerT0;
// uint8_t bClockStop; } __attribute__ ((packed));

#[test]
fn test_t0_get_parameters_reports_plain_waiting_integer() {
    // Spec-conformant behavior (issue #57 fixed): for T=0,
    // bWaitingIntegerT0 is a plain waiting integer per the quoted struct —
    // not a T=1-style (BWI<<4)|CWI pair — so GetParameters and
    // ResetParameters agree on the 0x00 default.
    // Given: a fresh reader with default parameters (BWI=4, CWI=13)
    let mut h = CcidMessageHandler::new(MockSmartcardDriver::new(), CHERRY_VID);

    // When: parameters are queried before any card activation (T=0)
    let gp = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_GET_PARAMETERS, 0, 1, [0; 3], &[]),
    );
    // Then: 5-byte T=0 structure whose bWaitingIntegerT0 is the plain
    // waiting integer default 0x00 — no T=1 BWI/CWI packing leaks in.
    assert_eq!(gp[9], 0, "bProtocolNum");
    let params = param_data(&gp);
    assert_eq!(params.len(), 5);
    assert_eq!(params[3], 0x00, "bWaitingIntegerT0");

    // And: ResetParameters answers with the same 0x00 — the two paths
    // agree.
    let rp = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_RESET_PARAMETERS, 0, 2, [0; 3], &[]),
    );
    assert_eq!(
        param_data(&rp)[3],
        0x00,
        "ResetParameters uses the same plain waiting integer"
    );
}

// CCID_SPEC: /* Section 6.1.4 */ struct ccid_pc_to_rdr_xfr_block {
// struct ccid_header hdr; uint8_t bBWI; uint16_t wLevelParameter;
// uint8_t abData[0]; } __attribute__ ((packed)); /* Response: RDR_to_PC_DataBlock */
#[test]
fn test_xfr_block_bwi_ignored_and_no_time_extension_reported() {
    // Given: an active T=0 card that answers any APDU with 90 00
    let mut h = CcidMessageHandler::new(
        MockSmartcardDriver::new()
            .card_present(true)
            .with_atr(&T0_ATR)
            .with_apdu_response(&[0x90, 0x00]),
        CHERRY_VID,
    );
    exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_ICC_POWER_ON, 0, 1, [0x00, 0, 0], &[]),
    );

    // When: an XfrBlock arrives with bBWI=5 in the header-specific field
    let resp = exchange(
        &mut h,
        &ccid_request(
            PC_TO_RDR_XFR_BLOCK,
            0,
            2,
            [0x05, 0, 0], // bBWI=5, wLevelParameter=0
            &[0x00, 0xA4, 0x04, 0x00, 0x00],
        ),
    );

    // Then: the response completes synchronously with cmd=OK — the bBWI
    // field is not consumed (transfers are blocking in this driver model),
    // and bmCommandStatus never reports TIME_EXTENSION. T=1 WTX requests
    // are answered inside t1_engine.rs (ARM-only, not host-testable here).
    assert_eq!(resp[0], RDR_TO_PC_DATABLOCK);
    assert_eq!(cmd_status(&resp), COMMAND_STATUS_NO_ERROR);
    assert_ne!(
        cmd_status(&resp),
        COMMAND_STATUS_TIME_EXTENSION,
        "synchronous transfers must not claim time extension"
    );
    assert_eq!(param_data(&resp), &[0x90, 0x00]);
}

// ---------------------------------------------------------------------------
// 4. T=0 APDU command + Secure (PIN pad) acceptance per profile
// ---------------------------------------------------------------------------

// CCID_SPEC: /* Section 6.1.10 */ struct ccid_pc_to_rdr_t0apdu {
// struct ccid_header hdr; uint8_t bmChanges; uint8_t bClassGetResponse;
// uint8_t bClassEnvelope; } __attribute__ ((packed));
// /* Response: RDR_to_PC_SlotStatus */
#[test]
fn test_t0_apdu_command_rejected_not_supported() {
    // Given: an idle reader operating at TPDU/Short-APDU exchange level
    let mut h = CcidMessageHandler::new(MockSmartcardDriver::new(), CHERRY_VID);

    // When: the host sends PC_to_RDR_T0APDU (T=0 APDU level tweaking)
    let resp = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_T0_APDU, 0, 0x33, [0x00, 0x00, 0x0C], &[]),
    );

    // Then: rejected with a SlotStatus carrying CMD_NOT_SUPPORTED — the
    // firmware answers XfrBlock transfers only, which matches the
    // TPDU/ShortAPDU dwFeatures exchange level of every device profile.
    assert_eq!(resp[0], RDR_TO_PC_SLOTSTATUS);
    assert_eq!(rseq(&resp), 0x33);
    assert_eq!(cmd_status(&resp), COMMAND_STATUS_FAILED);

    // CCID_SPEC: CCID_ERR_CMD_NOT_SUPPORTED = 0x00
    assert_eq!(berror(&resp), CCID_ERR_CMD_NOT_SUPPORTED);
}

// CCID_SPEC: struct ccid_pc_to_rdr_secure { struct ccid_header hdr;
// uint8_t bBWI; uint16_t wLevelParameter; uint8_t abData[0]; }
// __attribute__ ((packed)); struct ccid_pin_operation_data {
// uint8_t bPINOperation; uint8_t abPNDataStructure[0]; }
// __attribute__ ((packed));

/// Minimal valid PIN-verify data structure (CCID Rev 1.1 §6.1.11):
/// 16 bytes = 12-byte PIN probe header + 4-byte APDU head.
fn pin_verify_data() -> [u8; 16] {
    [
        30,   // bTimerOut (s)
        0x00, // bmFormatString
        0x00, // bmPINBlockString
        0x00, // bmPINLengthFormat
        4,    // wPINMaxExtraDigit lo = min length
        8,    // wPINMaxExtraDigit hi = max length
        0x02, // bEntryValidationCondition (key press)
        1,    // bNumberMessage
        0x09, 0x04, // wLangId (en-US)
        0x00, // bMsgIndex
        0x00, // bTeoPrologue
        // abPINApdu head (VERIFY template)
        0x00, 0x20, 0x00, 0x00,
    ]
}

#[test]
fn test_secure_pin_verify_rejected_on_non_pinpad_vendor() {
    // Spec-conformant behavior (issue #58 fixed): the Gemalto profiles
    // advertise bPINSupport=0 (no PIN pad; only the Cherry ST-2xxx profile
    // has one), so PC_to_RDR_Secure must be answered with
    // CMD_NOT_SUPPORTED instead of starting a silent async PIN-entry
    // session.
    // Given: an active card in a reader built with the Gemalto vendor id
    let mut h = CcidMessageHandler::new(
        MockSmartcardDriver::new()
            .card_present(true)
            .with_atr(&T0_ATR),
        GEMALTO_VID,
    );
    exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_ICC_POWER_ON, 0, 1, [0x00, 0, 0], &[]),
    );

    // When: the host sends PC_to_RDR_Secure with a PIN-verify operation
    let mut msg = ccid_request(PC_TO_RDR_SECURE, 0, 0x55, [0x00, 0, 0], &[]);
    msg.push(0x00); // bPINOperation = PIN verification
    msg.extend_from_slice(&pin_verify_data());
    let resp = exchange(&mut h, &msg);

    // Then: a DataBlock response with CMD_NOT_SUPPORTED is queued
    // synchronously and no PIN session is started.
    // CCID_SPEC: CCID_ERR_CMD_NOT_SUPPORTED = 0x00
    assert_eq!(resp[0], RDR_TO_PC_DATABLOCK);
    assert_eq!(rseq(&resp), 0x55);
    assert_eq!(cmd_status(&resp), COMMAND_STATUS_FAILED);
    assert_eq!(berror(&resp), CCID_ERR_CMD_NOT_SUPPORTED);
    assert!(!h.is_pin_verify_active());

    // And: the same message against the Cherry vendor id (the only
    // PIN-pad profile) is still accepted — the async PIN entry starts.
    let mut h = CcidMessageHandler::new(
        MockSmartcardDriver::new()
            .card_present(true)
            .with_atr(&T0_ATR),
        CHERRY_VID,
    );
    exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_ICC_POWER_ON, 0, 1, [0x00, 0, 0], &[]),
    );
    let resp = exchange(&mut h, &msg);
    assert!(resp.is_empty(), "PIN entry is asynchronous");
    assert!(h.is_pin_verify_active());
    h.complete_pin_entry(0x55, PinResult::Success, Some(&[0x90, 0x00]));
    let (len, out) = h.take_response();
    assert_eq!(len, CCID_HEADER_SIZE + 2);
    assert_eq!(out[0], RDR_TO_PC_DATABLOCK);
    assert_eq!(cmd_status(out), COMMAND_STATUS_NO_ERROR);
    assert!(!h.is_pin_verify_active());
}

// ---------------------------------------------------------------------------
// 5. Interrupt-IN NotifySlotChange semantics (STM32 USB path)
// ---------------------------------------------------------------------------

// CCID_SPEC: /* Section 6.3.1 */ struct ccid_rdr_to_pc_notify_slot_change {
// uint8_t bMessageType; uint8_t bmSlotCCState[0];
// /* as long as bNumSlots/4 padded to next byte */ } __attribute__ ((packed));

// CCID_SERIAL: Card insertion/withdrawal * 1 : RDR_to_PC_NotifySlotChange (0x50)
// * 1 : bmSlotIccState * 0x02 if card absent * 0x03 is card present

#[test]
fn test_notify_slot_change_slot0_bit_encoding() {
    // Given: any handler (encoding is a pure function of the slot state)
    let h = CcidMessageHandler::new(MockSmartcardDriver::new(), CHERRY_VID);

    // Then: slot 0 occupies bits [1:0] of bmSlotCCState — bit 0 = ICC
    // present, bit 1 = slot changed — i.e. insertion notifies 0x03 and
    // removal 0x02, matching the GemPC convention quoted above and the
    // reference USB CCID implementations (QEMU dev-smartcard-reader.c:
    // SLOT_0_STATE_MASK=1 / SLOT_0_CHANGED_MASK=2).
    assert_eq!(
        h.notify_slot_change_bytes(true, true),
        [RDR_TO_PC_NOTIFY_SLOT_CHANGE, 0x03]
    );
    assert_eq!(
        h.notify_slot_change_bytes(false, true),
        [RDR_TO_PC_NOTIFY_SLOT_CHANGE, 0x02]
    );
    assert_eq!(
        h.notify_slot_change_bytes(true, false),
        [RDR_TO_PC_NOTIFY_SLOT_CHANGE, 0x01]
    );
    assert_eq!(
        h.notify_slot_change_bytes(false, false),
        [RDR_TO_PC_NOTIFY_SLOT_CHANGE, 0x00]
    );
}

#[test]
fn test_card_insertion_notifies_once_and_lands_inactive() {
    // Given: a reader that booted with an empty slot
    let mut h = CcidMessageHandler::new(FlippableDriver::new(false, &T0_ATR), CHERRY_VID);
    assert_eq!(h.slot_state(), SlotState::Absent);

    // When: a card is inserted
    h.driver_mut().flip(true);
    let changed = h.check_card_presence();

    // Then: the change is reported exactly once, the slot lands
    // PresentInactive (not activated), and the interrupt byte the USB poll
    // loop would send encodes insertion as 0x03.
    assert_eq!(changed, Some(true));
    assert_eq!(h.slot_state(), SlotState::PresentInactive);
    assert_eq!(h.get_icc_status(), ICC_STATUS_PRESENT_INACTIVE);
    assert_eq!(
        h.notify_slot_change_bytes(h.is_card_present(), true),
        [RDR_TO_PC_NOTIFY_SLOT_CHANGE, 0x03]
    );
    assert_eq!(h.check_card_presence(), None, "no second edge");
}

// ---------------------------------------------------------------------------
// 6. Mid-transfer card removal
// ---------------------------------------------------------------------------

#[test]
fn test_card_removal_resets_slot_and_powers_off_driver() {
    // Given: an activated card session
    let mut h = CcidMessageHandler::new(FlippableDriver::new(true, &T0_ATR), CHERRY_VID);
    let on = exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_ICC_POWER_ON, 0, 1, [0x00, 0, 0], &[]),
    );
    assert_eq!(icc_status(&on), ICC_STATUS_PRESENT_ACTIVE);

    // When: the card is yanked
    h.driver_mut().flip(false);
    assert_eq!(h.check_card_presence(), Some(true));

    // Then: the slot reports absent and the driver was powered off.
    assert_eq!(h.slot_state(), SlotState::Absent);
    assert_eq!(h.get_icc_status(), ICC_STATUS_NO_ICC);
    assert_eq!(h.driver().power_off_count, 1);
}

#[test]
fn test_card_removal_discards_pending_inbound_message() {
    // Given: an activated session with a complete XfrBlock already buffered
    let mut h = CcidMessageHandler::new(FlippableDriver::new(true, &T0_ATR), CHERRY_VID);
    exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_ICC_POWER_ON, 0, 1, [0x00, 0, 0], &[]),
    );
    h.set_rx_data(&ccid_request(
        PC_TO_RDR_XFR_BLOCK,
        0,
        2,
        [0; 3],
        &[0x00, 0xA4, 0x04, 0x00, 0x00],
    ));
    assert!(h.message_ready());

    // When: the card disappears before the message is handled
    h.driver_mut().flip(false);
    h.check_card_presence();

    // Then: the inbound message is dropped (rx buffer cleared) and handling
    // it afterwards is a no-op — no response, no panic.
    assert!(!h.message_ready());
    h.handle_message();
    assert_eq!(h.get_tx_len(), 0);
    let (len, _) = h.take_response();
    assert_eq!(len, 0);
}

#[test]
fn test_card_removal_clears_cmd_busy() {
    // Given: a handled command awaiting response pickup (cmd_busy set)
    let mut h = CcidMessageHandler::new(FlippableDriver::new(true, &T0_ATR), CHERRY_VID);
    h.set_rx_data(&ccid_request(PC_TO_RDR_GET_SLOT_STATUS, 0, 9, [0; 3], &[]));
    h.handle_message();
    assert!(h.cmd_busy());

    // When: the card is removed
    h.driver_mut().flip(false);
    h.check_card_presence();

    // Then: the busy latch is released so the reader accepts new commands.
    assert!(!h.cmd_busy());
}

#[test]
fn test_card_removal_mid_pin_entry_resets_secure_state() {
    // Given: an active session with a PIN-verify entry in progress
    let mut h = CcidMessageHandler::new(FlippableDriver::new(true, &T0_ATR), CHERRY_VID);
    exchange(
        &mut h,
        &ccid_request(PC_TO_RDR_ICC_POWER_ON, 0, 1, [0x00, 0, 0], &[]),
    );
    let mut msg = ccid_request(PC_TO_RDR_SECURE, 0, 0x77, [0x00, 0, 0], &[]);
    msg.push(0x00);
    msg.extend_from_slice(&pin_verify_data());
    exchange(&mut h, &msg);
    assert!(h.is_pin_verify_active());

    // When: the card is removed mid-entry
    h.driver_mut().flip(false);
    h.check_card_presence();

    // Then: the secure session is dropped — a late completion still emits
    // the DataBlock for the stored bSeq, but now with ICC status absent
    // and no card exchange behind it (honest characterization of the
    // completion path; the USB main loop drops pin state on removal).
    assert!(!h.is_pin_verify_active());
    h.complete_pin_entry(0x77, PinResult::Success, None);
    let (len, out) = h.take_response();
    assert_eq!(len, CCID_HEADER_SIZE);
    assert_eq!(out[0], RDR_TO_PC_DATABLOCK);
    assert_eq!(rseq(out), 0x77);
    assert_eq!(cmd_status(out), COMMAND_STATUS_NO_ERROR);
    assert_eq!(icc_status(out), ICC_STATUS_NO_ICC);
}
