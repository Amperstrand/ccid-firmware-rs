use ccid_firmware_rs::ccid_core::{
    CcidMessageHandler, SlotState, CCID_HEADER_SIZE, COMMAND_STATUS_FAILED,
    COMMAND_STATUS_NO_ERROR, COMMAND_STATUS_TIME_EXTENSION, ICC_STATUS_NO_ICC,
    ICC_STATUS_PRESENT_ACTIVE, ICC_STATUS_PRESENT_INACTIVE, PC_TO_RDR_ABORT, PC_TO_RDR_ESCAPE,
    PC_TO_RDR_GET_PARAMETERS, PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_CLOCK,
    PC_TO_RDR_ICC_POWER_OFF, PC_TO_RDR_ICC_POWER_ON, PC_TO_RDR_MECHANICAL, PC_TO_RDR_SECURE,
    PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ, PC_TO_RDR_SET_PARAMETERS, PC_TO_RDR_T0_APDU,
    PC_TO_RDR_XFR_BLOCK, RDR_TO_PC_DATABLOCK, RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ, RDR_TO_PC_ESCAPE,
    RDR_TO_PC_PARAMETERS, RDR_TO_PC_SLOTSTATUS,
};
use ccid_firmware_rs::mock_driver::{MockCall, MockSmartcardDriver};

pub struct CcidTestHarness {
    handler: CcidMessageHandler<MockSmartcardDriver>,
}

impl CcidTestHarness {
    pub fn new(driver: MockSmartcardDriver, vendor_id: u16) -> Self {
        Self {
            handler: CcidMessageHandler::new(driver, vendor_id),
        }
    }

    pub fn handler(&self) -> &CcidMessageHandler<MockSmartcardDriver> {
        &self.handler
    }

    pub fn handler_mut(&mut self) -> &mut CcidMessageHandler<MockSmartcardDriver> {
        &mut self.handler
    }

    pub fn send(&mut self, msg_type: u8, payload: &[u8], seq: u8) -> Vec<u8> {
        let mut msg = vec![0u8; CCID_HEADER_SIZE + payload.len()];
        msg[0] = msg_type;
        let len = payload.len() as u32;
        msg[1..5].copy_from_slice(&len.to_le_bytes());
        msg[5] = 0; // slot
        msg[6] = seq;
        msg[7] = 0; // bBWI
        if payload.len() > 2 {
            msg[8] = payload[0]; // wLevelParameter low
            msg[9] = payload[1]; // wLevelParameter high
        }
        msg[CCID_HEADER_SIZE..].copy_from_slice(payload);
        self.handler.set_rx_data(&msg);
        self.handler.handle_message();
        let (_len, data) = self.handler.take_response();
        data.to_vec()
    }

    pub fn send_raw(&mut self, ccid_bytes: &[u8]) -> Vec<u8> {
        self.handler.set_rx_data(ccid_bytes);
        self.handler.handle_message();
        let (_len, data) = self.handler.take_response();
        data.to_vec()
    }

    pub fn insert_card(&mut self, atr: &[u8]) {
        let driver = self.handler.driver_mut();
        // We can't mutate the driver through the trait ref,
        // so we need to rebuild the handler
    }

    pub fn simulate_card_insert(&mut self, atr: &[u8]) {
        self.handler.set_card_present(true);
    }

    pub fn simulate_card_remove(&mut self) {
        self.handler.set_card_present(false);
    }

    pub fn call_log(&self) -> &[MockCall] {
        self.handler.driver().call_log()
    }
}

pub fn gemalto_ct30() -> CcidTestHarness {
    let driver = MockSmartcardDriver::new().card_present(true);
    CcidTestHarness::new(driver, 0x08E6)
}

pub fn gemalto_ct30_with_atr(atr: &[u8]) -> CcidTestHarness {
    let driver = MockSmartcardDriver::new().card_present(true).with_atr(atr);
    CcidTestHarness::new(driver, 0x08E6)
}

pub fn cherry_st2xxx() -> CcidTestHarness {
    let driver = MockSmartcardDriver::new().card_present(true);
    CcidTestHarness::new(driver, 0x046A)
}

pub fn no_card() -> CcidTestHarness {
    let driver = MockSmartcardDriver::new().card_present(false);
    CcidTestHarness::new(driver, 0x08E6)
}

pub fn parse_ccid_response(resp: &[u8]) -> CcidResponse {
    if resp.len() < CCID_HEADER_SIZE {
        return CcidResponse {
            msg_type: 0,
            data_len: 0,
            slot: 0,
            seq: 0,
            b_status: 0,
            b_error: 0,
            b_clock_status: 0,
            data: vec![],
        };
    }
    let data_len = u32::from_le_bytes([resp[1], resp[2], resp[3], resp[4]]) as usize;
    CcidResponse {
        msg_type: resp[0],
        data_len,
        slot: resp[5],
        seq: resp[6],
        b_status: resp[7],
        b_error: resp[8],
        b_clock_status: resp[9],
        data: resp[CCID_HEADER_SIZE..].to_vec(),
    }
}

pub struct CcidResponse {
    pub msg_type: u8,
    pub data_len: usize,
    pub slot: u8,
    pub seq: u8,
    pub b_status: u8,
    pub b_error: u8,
    pub b_clock_status: u8,
    pub data: Vec<u8>,
}

impl CcidResponse {
    pub fn cmd_status(&self) -> u8 {
        (self.b_status >> 6) & 0x03
    }

    pub fn icc_status(&self) -> u8 {
        self.b_status & 0x03
    }

    pub fn is_success(&self) -> bool {
        self.cmd_status() == COMMAND_STATUS_NO_ERROR && self.b_error == 0
    }

    pub fn is_cmd_failed(&self) -> bool {
        self.cmd_status() == COMMAND_STATUS_FAILED
    }
}

pub fn msg_name(msg_type: u8) -> &'static str {
    match msg_type {
        PC_TO_RDR_ICC_POWER_ON => "PC_TO_RDR_ICC_POWER_ON",
        PC_TO_RDR_ICC_POWER_OFF => "PC_TO_RDR_ICC_POWER_OFF",
        PC_TO_RDR_GET_SLOT_STATUS => "PC_TO_RDR_GET_SLOT_STATUS",
        PC_TO_RDR_XFR_BLOCK => "PC_TO_RDR_XFR_BLOCK",
        PC_TO_RDR_GET_PARAMETERS => "PC_TO_RDR_GET_PARAMETERS",
        PC_TO_RDR_SET_PARAMETERS => "PC_TO_RDR_SET_PARAMETERS",
        PC_TO_RDR_RESET_PARAMETERS => "PC_TO_RDR_RESET_PARAMETERS",
        PC_TO_RDR_SECURE => "PC_TO_RDR_SECURE",
        PC_TO_RDR_T0_APDU => "PC_TO_RDR_T0_APDU",
        PC_TO_RDR_ESCAPE => "PC_TO_RDR_ESCAPE",
        PC_TO_RDR_ICC_CLOCK => "PC_TO_RDR_ICC_CLOCK",
        PC_TO_RDR_MECHANICAL => "PC_TO_RDR_MECHANICAL",
        PC_TO_RDR_ABORT => "PC_TO_RDR_ABORT",
        PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ => "PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ",
        RDR_TO_PC_DATABLOCK => "RDR_TO_PC_DATABLOCK",
        RDR_TO_PC_SLOTSTATUS => "RDR_TO_PC_SLOTSTATUS",
        RDR_TO_PC_PARAMETERS => "RDR_TO_PC_PARAMETERS",
        RDR_TO_PC_ESCAPE => "RDR_TO_PC_ESCAPE",
        RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ => "RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ",
        _ => "UNKNOWN",
    }
}
