#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_is_multiple_of)]

use crate::driver::SmartcardDriver;
use crate::pinpad::{
    ModifyApduBuilder, PinBuffer, PinModifyParams, PinResult, PinVerifyParams, VerifyApduBuilder,
};
use crate::protocol_unit::{parse_atr, AtrParams};

pub const PC_TO_RDR_ICC_POWER_ON: u8 = 0x62;
pub const PC_TO_RDR_ICC_POWER_OFF: u8 = 0x63;
pub const PC_TO_RDR_GET_SLOT_STATUS: u8 = 0x65;
pub const PC_TO_RDR_XFR_BLOCK: u8 = 0x6F;
pub const PC_TO_RDR_GET_PARAMETERS: u8 = 0x6C;
pub const PC_TO_RDR_SET_PARAMETERS: u8 = 0x61;
pub const PC_TO_RDR_SECURE: u8 = 0x69;
pub const PC_TO_RDR_T0_APDU: u8 = 0x6A;
pub const PC_TO_RDR_ESCAPE: u8 = 0x6B;
pub const PC_TO_RDR_RESET_PARAMETERS: u8 = 0x6D;
pub const PC_TO_RDR_ICC_CLOCK: u8 = 0x6E;
pub const PC_TO_RDR_MECHANICAL: u8 = 0x71;
pub const PC_TO_RDR_ABORT: u8 = 0x72;
pub const PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ: u8 = 0x73;

pub const RDR_TO_PC_DATABLOCK: u8 = 0x80;
pub const RDR_TO_PC_SLOTSTATUS: u8 = 0x81;
pub const RDR_TO_PC_PARAMETERS: u8 = 0x82;
pub const RDR_TO_PC_ESCAPE: u8 = 0x83;
pub const RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ: u8 = 0x84;
pub const RDR_TO_PC_NOTIFY_SLOT_CHANGE: u8 = 0x50;

pub const REQUEST_ABORT: u8 = 0x01;
pub const REQUEST_GET_CLOCK_FREQUENCIES: u8 = 0x02;
pub const REQUEST_GET_DATA_RATES: u8 = 0x03;

pub const PACKET_SIZE: usize = 64;
pub const CCID_HEADER_SIZE: usize = 10;
pub const MAX_CCID_MESSAGE_LENGTH: usize = 271;

pub const ICC_STATUS_PRESENT_ACTIVE: u8 = 0x00;
pub const ICC_STATUS_PRESENT_INACTIVE: u8 = 0x01;
pub const ICC_STATUS_NO_ICC: u8 = 0x02;

pub const COMMAND_STATUS_NO_ERROR: u8 = 0x00;
pub const COMMAND_STATUS_FAILED: u8 = 0x01;
pub const COMMAND_STATUS_TIME_EXTENSION: u8 = 0x02;

pub const CCID_ERR_CMD_NOT_SUPPORTED: u8 = 0x00;
pub const CCID_ERR_CMD_SLOT_BUSY: u8 = 0xE0;
pub const CCID_ERR_PIN_CANCELLED: u8 = 0xEF;
pub const CCID_ERR_PIN_TIMEOUT: u8 = 0xF0;
pub const CCID_ERR_BUSY_WITH_AUTO_SEQUENCE: u8 = 0xF2;
pub const CCID_ERR_DEACTIVATED_PROTOCOL: u8 = 0xF3;
pub const CCID_ERR_PROCEDURE_BYTE_CONFLICT: u8 = 0xF4;
pub const CCID_ERR_ICC_CLASS_NOT_SUPPORTED: u8 = 0xF5;
pub const CCID_ERR_ICC_PROTOCOL_NOT_SUPPORTED: u8 = 0xF6;
pub const CCID_ERR_BAD_ATR_TCK: u8 = 0xF7;
pub const CCID_ERR_BAD_ATR_TS: u8 = 0xF8;
pub const CCID_ERR_HW_ERROR: u8 = 0xFB;
pub const CCID_ERR_XFR_OVERRUN: u8 = 0xFC;
pub const CCID_ERR_XFR_PARITY_ERROR: u8 = 0xFD;
pub const CCID_ERR_ICC_MUTE: u8 = 0xFE;
pub const CCID_ERR_CMD_ABORTED: u8 = 0xFF;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SlotState {
    Absent,
    PresentInactive,
    PresentActive,
}

#[derive(Clone)]
pub enum SecureState {
    Idle,
    WaitingForPinVerify { seq: u8, params: PinVerifyParams },
    WaitingForPinModify { seq: u8, params: PinModifyParams },
}

pub struct CcidMessageHandler<D: SmartcardDriver> {
    driver: D,
    rx_buffer: [u8; MAX_CCID_MESSAGE_LENGTH],
    rx_len: usize,
    tx_buffer: [u8; MAX_CCID_MESSAGE_LENGTH],
    tx_len: usize,
    slot_state: SlotState,
    cmd_busy: bool,
    card_present_last: bool,
    current_protocol: u8,
    atr_params: AtrParams,
    secure_state: SecureState,
    response_buffer: [u8; 261],
    vendor_id: u16,
    #[cfg(feature = "display")]
    pin_result_pending: Option<(u8, PinResult, PinBuffer, PinVerifyParams)>,
    #[cfg(feature = "display")]
    pin_modify_result_pending: Option<(u8, PinResult, PinBuffer, PinBuffer, PinModifyParams)>,
}

impl<D: SmartcardDriver> CcidMessageHandler<D> {
    pub fn new(driver: D, vendor_id: u16) -> Self {
        let mut this = Self {
            driver,
            rx_buffer: [0u8; MAX_CCID_MESSAGE_LENGTH],
            rx_len: 0,
            tx_buffer: [0u8; MAX_CCID_MESSAGE_LENGTH],
            tx_len: 0,
            slot_state: SlotState::Absent,
            cmd_busy: false,
            card_present_last: false,
            current_protocol: 0,
            atr_params: AtrParams::default(),
            secure_state: SecureState::Idle,
            response_buffer: [0u8; 261],
            vendor_id,
            #[cfg(feature = "display")]
            pin_result_pending: None,
            #[cfg(feature = "display")]
            pin_modify_result_pending: None,
        };
        let present = this.driver.is_card_present();
        ccid_info!("CCID init: card_present={}", present);
        if present {
            this.card_present_last = true;
            this.slot_state = SlotState::PresentInactive;
        }
        this
    }

    pub fn driver(&self) -> &D {
        &self.driver
    }

    pub fn driver_mut(&mut self) -> &mut D {
        &mut self.driver
    }

    pub fn feed(&mut self, data: &[u8]) {
        let remaining = self.rx_buffer.len() - self.rx_len;
        let copy_len = data.len().min(remaining);
        self.rx_buffer[self.rx_len..self.rx_len + copy_len].copy_from_slice(&data[..copy_len]);
        self.rx_len += copy_len;
    }

    pub fn message_ready(&self) -> bool {
        if self.rx_len < CCID_HEADER_SIZE {
            return false;
        }
        let msg_len = u32::from_le_bytes([
            self.rx_buffer[1],
            self.rx_buffer[2],
            self.rx_buffer[3],
            self.rx_buffer[4],
        ]) as usize;
        let total_len = CCID_HEADER_SIZE + msg_len;
        self.rx_len >= total_len
    }

    pub fn handle_message(&mut self) {
        if self.rx_len < CCID_HEADER_SIZE {
            ccid_warn!("CCID: message too short");
            return;
        }

        let msg_type = self.rx_buffer[0];
        let slot = self.rx_buffer[5];
        let seq = self.rx_buffer[6];

        ccid_debug!(
            "CCID: received message type=0x{:02X}, slot={}, seq={}",
            msg_type,
            slot,
            seq
        );

        if slot != 0 {
            self.send_slot_status(seq, COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC, 0x05);
            return;
        }

        if self.cmd_busy {
            self.send_slot_status(
                seq,
                COMMAND_STATUS_FAILED,
                self.get_icc_status(),
                CCID_ERR_CMD_SLOT_BUSY,
            );
            return;
        }
        self.cmd_busy = true;

        match msg_type {
            PC_TO_RDR_GET_SLOT_STATUS => self.handle_get_slot_status(seq),
            PC_TO_RDR_ICC_POWER_ON => self.handle_power_on(seq),
            PC_TO_RDR_ICC_POWER_OFF => self.handle_power_off(seq),
            PC_TO_RDR_XFR_BLOCK => self.handle_xfr_block(seq),
            PC_TO_RDR_GET_PARAMETERS => self.handle_get_parameters(seq),
            PC_TO_RDR_SET_PARAMETERS => self.handle_set_parameters(seq),
            PC_TO_RDR_RESET_PARAMETERS => {
                self.handle_reset_parameters(seq);
            }
            PC_TO_RDR_ESCAPE => self.handle_escape(seq),
            PC_TO_RDR_ICC_CLOCK => {
                self.handle_icc_clock(seq);
            }
            PC_TO_RDR_T0_APDU => {
                ccid_debug!("CCID: T0APDU command (stub - TPDU level sufficient)");
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
            PC_TO_RDR_SECURE => {
                self.handle_secure(seq);
            }
            PC_TO_RDR_MECHANICAL => {
                ccid_debug!("CCID: Mechanical command (stub - no mechanical parts)");
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
            PC_TO_RDR_ABORT => {
                ccid_debug!("CCID: Abort command (stub - single-slot sequential execution)");
                self.send_slot_status(seq, COMMAND_STATUS_NO_ERROR, self.get_icc_status(), 0);
            }
            PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ => {
                self.handle_set_data_rate_and_clock(seq);
            }
            _ => {
                ccid_warn!("CCID: unknown message type 0x{:02X}", msg_type);
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
        }
    }

    pub fn take_response(&mut self) -> (usize, &[u8]) {
        let len = self.tx_len;
        self.rx_len = 0;
        self.tx_len = 0;
        self.cmd_busy = false;
        (len, &self.tx_buffer[..len])
    }

    pub fn check_card_presence(&mut self) -> Option<bool> {
        let present_now = self.driver.is_card_present();
        if present_now != self.card_present_last {
            ccid_info!(
                "Card state change: {} -> {}",
                self.card_present_last,
                present_now
            );
            self.card_present_last = present_now;
            if !present_now {
                ccid_info!("Card removed, powering off driver");
                self.driver.power_off();
                self.slot_state = SlotState::Absent;
                self.cmd_busy = false;
                self.rx_len = 0;
                self.secure_state = SecureState::Idle;
                #[cfg(feature = "display")]
                {
                    self.pin_result_pending = None;
                }
            } else {
                self.slot_state = SlotState::PresentInactive;
            }
            return Some(true);
        }
        None
    }

    pub fn get_icc_status(&self) -> u8 {
        match self.slot_state {
            SlotState::PresentActive => ICC_STATUS_PRESENT_ACTIVE,
            SlotState::PresentInactive => ICC_STATUS_PRESENT_INACTIVE,
            SlotState::Absent => ICC_STATUS_NO_ICC,
        }
    }

    pub fn notify_slot_change_bytes(&self, card_present: bool, changed: bool) -> [u8; 2] {
        let mut bits: u8 = 0;
        if card_present {
            bits |= 0x01;
        }
        if changed {
            bits |= 0x02;
        }
        [RDR_TO_PC_NOTIFY_SLOT_CHANGE, bits]
    }

    fn build_status(cmd_status: u8, icc_status: u8) -> u8 {
        (cmd_status << 6) | icc_status
    }

    fn send_err_resp(&mut self, msg_type: u8, seq: u8, error: u8) {
        let icc = self.get_icc_status();
        let status = Self::build_status(COMMAND_STATUS_FAILED, icc);
        match msg_type {
            PC_TO_RDR_ICC_POWER_ON | PC_TO_RDR_XFR_BLOCK | PC_TO_RDR_SECURE => {
                self.tx_buffer[0] = RDR_TO_PC_DATABLOCK;
                self.tx_buffer[1..5].copy_from_slice(&0u32.to_le_bytes());
                self.tx_buffer[5] = 0;
                self.tx_buffer[6] = seq;
                self.tx_buffer[7] = status;
                self.tx_buffer[8] = error;
                self.tx_buffer[9] = 0;
                self.tx_len = CCID_HEADER_SIZE;
            }
            PC_TO_RDR_ICC_POWER_OFF
            | PC_TO_RDR_GET_SLOT_STATUS
            | PC_TO_RDR_ICC_CLOCK
            | PC_TO_RDR_T0_APDU
            | PC_TO_RDR_MECHANICAL
            | PC_TO_RDR_ABORT
            | PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ => {
                self.send_slot_status(seq, COMMAND_STATUS_FAILED, icc, error);
            }
            PC_TO_RDR_GET_PARAMETERS | PC_TO_RDR_RESET_PARAMETERS | PC_TO_RDR_SET_PARAMETERS => {
                self.tx_buffer[0] = RDR_TO_PC_PARAMETERS;
                self.tx_buffer[1..5].copy_from_slice(&0u32.to_le_bytes());
                self.tx_buffer[5] = 0;
                self.tx_buffer[6] = seq;
                self.tx_buffer[7] = status;
                self.tx_buffer[8] = error;
                self.tx_buffer[9] = 0;
                self.tx_len = CCID_HEADER_SIZE;
            }
            PC_TO_RDR_ESCAPE => {
                self.tx_buffer[0] = RDR_TO_PC_ESCAPE;
                self.tx_buffer[1..5].copy_from_slice(&0u32.to_le_bytes());
                self.tx_buffer[5] = 0;
                self.tx_buffer[6] = seq;
                self.tx_buffer[7] = status;
                self.tx_buffer[8] = error;
                self.tx_buffer[9] = 0;
                self.tx_len = CCID_HEADER_SIZE;
            }
            _ => {
                self.send_slot_status(seq, COMMAND_STATUS_FAILED, icc, CCID_ERR_CMD_NOT_SUPPORTED);
            }
        }
    }

    fn handle_power_on(&mut self, seq: u8) {
        let data_len = u32::from_le_bytes([
            self.rx_buffer[1],
            self.rx_buffer[2],
            self.rx_buffer[3],
            self.rx_buffer[4],
        ]);
        if data_len != 0 {
            ccid_warn!("CCID: IccPowerOn with non-zero dwLength={}", data_len);
            self.send_slot_status(
                seq,
                COMMAND_STATUS_FAILED,
                self.get_icc_status(),
                CCID_ERR_CMD_NOT_SUPPORTED,
            );
            return;
        }

        if !self.driver.is_card_present() {
            self.send_slot_status(
                seq,
                COMMAND_STATUS_FAILED,
                ICC_STATUS_NO_ICC,
                CCID_ERR_ICC_MUTE,
            );
            return;
        }

        let power_select = if self.rx_len > 7 {
            self.rx_buffer[7]
        } else {
            0
        };
        if power_select == 0x02 || power_select == 0x03 {
            self.send_slot_status(
                seq,
                COMMAND_STATUS_FAILED,
                ICC_STATUS_PRESENT_INACTIVE,
                CCID_ERR_CMD_NOT_SUPPORTED,
            );
            return;
        }

        match self.driver.power_on() {
            Ok(atr) => {
                self.slot_state = SlotState::PresentActive;
                self.atr_params = parse_atr(atr);
                self.current_protocol = self.atr_params.protocol;
                let atr_len = atr.len().min(MAX_CCID_MESSAGE_LENGTH - CCID_HEADER_SIZE);

                self.tx_buffer[0] = RDR_TO_PC_DATABLOCK;
                self.tx_buffer[1..5].copy_from_slice(&(atr_len as u32).to_le_bytes());
                self.tx_buffer[5] = 0;
                self.tx_buffer[6] = seq;
                self.tx_buffer[7] =
                    Self::build_status(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE);
                self.tx_buffer[8] = 0;
                self.tx_buffer[9] = 0;

                self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + atr_len]
                    .copy_from_slice(&atr[..atr_len]);

                self.tx_len = CCID_HEADER_SIZE + atr_len;
                ccid_info!("CCID: PowerOn success, ATR len={}", atr_len);
            }
            Err(_e) => {
                ccid_error!("CCID: PowerOn failed");
                self.slot_state = SlotState::PresentInactive;
                self.send_slot_status(
                    seq,
                    COMMAND_STATUS_FAILED,
                    ICC_STATUS_PRESENT_INACTIVE,
                    CCID_ERR_ICC_MUTE,
                );
            }
        }
    }

    fn handle_reset_parameters(&mut self, seq: u8) {
        self.atr_params = AtrParams::default();
        self.current_protocol = 0;

        let params: [u8; 5] = [
            0x11, // bmFindexDindex (Fi=372, Di=1)
            0x00, // bmTCCKST0
            0x00, // bGuardTimeT0
            0x00, // bWaitingIntegerT0
            0x00, // bClockStop
        ];
        self.tx_buffer[0] = RDR_TO_PC_PARAMETERS;
        self.tx_buffer[1..5].copy_from_slice(&5u32.to_le_bytes());
        self.tx_buffer[5] = 0;
        self.tx_buffer[6] = seq;
        self.tx_buffer[7] = Self::build_status(COMMAND_STATUS_NO_ERROR, self.get_icc_status());
        self.tx_buffer[8] = 0;
        self.tx_buffer[9] = 0;
        self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + 5].copy_from_slice(&params);
        self.tx_len = CCID_HEADER_SIZE + 5;
    }

    fn handle_set_data_rate_and_clock(&mut self, seq: u8) {
        const MIN_LEN: usize = 10 + 8;
        if self.rx_len < MIN_LEN {
            self.send_slot_status(
                seq,
                COMMAND_STATUS_FAILED,
                self.get_icc_status(),
                CCID_ERR_CMD_NOT_SUPPORTED,
            );
            return;
        }
        let clock_hz = u32::from_le_bytes([
            self.rx_buffer[10],
            self.rx_buffer[11],
            self.rx_buffer[12],
            self.rx_buffer[13],
        ]);
        let rate_bps = u32::from_le_bytes([
            self.rx_buffer[14],
            self.rx_buffer[15],
            self.rx_buffer[16],
            self.rx_buffer[17],
        ]);
        match self.driver.set_clock_and_rate(clock_hz, rate_bps) {
            Ok((actual_clock, actual_rate)) => {
                self.tx_buffer[0] = RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ;
                self.tx_buffer[1..5].copy_from_slice(&8u32.to_le_bytes());
                self.tx_buffer[5] = 0;
                self.tx_buffer[6] = seq;
                self.tx_buffer[7] =
                    Self::build_status(COMMAND_STATUS_NO_ERROR, self.get_icc_status());
                self.tx_buffer[8] = 0;
                self.tx_buffer[9] = 0;
                self.tx_buffer[10..14].copy_from_slice(&actual_clock.to_le_bytes());
                self.tx_buffer[14..18].copy_from_slice(&actual_rate.to_le_bytes());
                self.tx_len = CCID_HEADER_SIZE + 8;
            }
            Err(_) => {
                self.send_slot_status(
                    seq,
                    COMMAND_STATUS_FAILED,
                    self.get_icc_status(),
                    CCID_ERR_HW_ERROR,
                );
            }
        }
    }

    fn handle_icc_clock(&mut self, seq: u8) {
        let icc = self.get_icc_status();
        if icc != ICC_STATUS_PRESENT_ACTIVE {
            self.send_slot_status_with_clock(seq, COMMAND_STATUS_FAILED, icc, CCID_ERR_ICC_MUTE, 0);
            return;
        }
        let clock_command = if self.rx_len > 7 {
            self.rx_buffer[7]
        } else {
            0
        };
        let enable = clock_command == 0;
        self.driver.set_clock(enable);
        let b_clock_status: u8 = if enable { 0x00 } else { 0x01 };
        self.send_slot_status_with_clock(seq, COMMAND_STATUS_NO_ERROR, icc, 0, b_clock_status);
    }

    fn handle_power_off(&mut self, seq: u8) {
        self.driver.power_off();
        self.slot_state = SlotState::PresentInactive;
        self.current_protocol = 0;

        self.tx_buffer[0] = RDR_TO_PC_SLOTSTATUS;
        self.tx_buffer[1..5].copy_from_slice(&0u32.to_le_bytes());
        self.tx_buffer[5] = 0;
        self.tx_buffer[6] = seq;
        self.tx_buffer[7] =
            Self::build_status(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_INACTIVE);
        self.tx_buffer[8] = 0;
        self.tx_buffer[9] = 0;

        self.tx_len = CCID_HEADER_SIZE;
        ccid_info!("CCID: PowerOff");
    }

    fn handle_get_slot_status(&mut self, seq: u8) {
        let icc_status = self.get_icc_status();
        ccid_info!(
            "GetSlotStatus: slot_state={} icc={}",
            self.slot_state as u8,
            icc_status
        );
        self.send_slot_status(seq, COMMAND_STATUS_NO_ERROR, icc_status, 0);
    }

    fn intercept_xfr_special(&mut self, data: &[u8], data_len: usize, seq: u8) -> bool {
        if (3..=5).contains(&data_len) && data[0] == 0xFF {
            let pps0 = data[1];
            if (pps0 & 0xE0) == 0x00 && pps0 != 0x00 {
                ccid_info!("CCID: XfrBlock PPS request intercepted");
                let status = Self::build_status(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE);
                self.tx_buffer[0] = RDR_TO_PC_DATABLOCK;
                self.tx_buffer[1..5].copy_from_slice(&(data_len as u32).to_le_bytes());
                self.tx_buffer[5] = 0;
                self.tx_buffer[6] = seq;
                self.tx_buffer[7] = status;
                self.tx_buffer[8] = 0;
                self.tx_buffer[9] = 0;
                self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + data_len]
                    .copy_from_slice(&data[..data_len]);
                self.tx_len = CCID_HEADER_SIZE + data_len;
                return true;
            }
        }

        if data_len == 5 && data[0] == 0x00 && (data[1] == 0xC1 || data[1] == 0xE1) {
            let lrc_check: u8 = data.iter().take(4).fold(0u8, |a, &b| a ^ b);
            if lrc_check == data[4] {
                ccid_info!("CCID: XfrBlock S(IFS) request intercepted");
                let mut resp = [0u8; 5];
                resp[0] = 0x00;
                resp[1] = if data[1] == 0xC1 { 0xE1 } else { 0xC1 };
                resp[2] = data[2];
                resp[3] = data[3];
                resp[4] = 0;
                resp[4] = resp.iter().fold(0u8, |a, &b| a ^ b);
                let status = Self::build_status(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE);
                self.tx_buffer[0] = RDR_TO_PC_DATABLOCK;
                self.tx_buffer[1..5].copy_from_slice(&5u32.to_le_bytes());
                self.tx_buffer[5] = 0;
                self.tx_buffer[6] = seq;
                self.tx_buffer[7] = status;
                self.tx_buffer[8] = 0;
                self.tx_buffer[9] = 0;
                self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + 5].copy_from_slice(&resp);
                self.tx_len = CCID_HEADER_SIZE + 5;
                return true;
            }
        }

        false
    }

    fn handle_xfr_block(&mut self, seq: u8) {
        if self.slot_state != SlotState::PresentActive {
            self.send_slot_status(seq, COMMAND_STATUS_FAILED, self.get_icc_status(), 0xFE);
            return;
        }

        let data_len = u32::from_le_bytes([
            self.rx_buffer[1],
            self.rx_buffer[2],
            self.rx_buffer[3],
            self.rx_buffer[4],
        ]) as usize;

        if data_len > 261 || data_len > MAX_CCID_MESSAGE_LENGTH - CCID_HEADER_SIZE {
            ccid_warn!("CCID: XfrBlock Extended APDU rejected");
            self.send_slot_status(seq, COMMAND_STATUS_FAILED, ICC_STATUS_PRESENT_ACTIVE, 0x07);
            return;
        }

        let data_start = CCID_HEADER_SIZE;
        let data_end = CCID_HEADER_SIZE + data_len;
        let mut apdu_buf = [0u8; 261];
        let copy_len = data_len.min(261);
        apdu_buf[..copy_len].copy_from_slice(&self.rx_buffer[data_start..data_end]);

        if self.intercept_xfr_special(&apdu_buf[..copy_len], copy_len, seq) {
            return;
        }

        ccid_info!("CCID: XfrBlock APDU len={}", copy_len,);
        let mut response_buf = [0u8; MAX_CCID_MESSAGE_LENGTH - CCID_HEADER_SIZE];
        let resp_len: usize;

        match self
            .driver
            .transmit_apdu(&apdu_buf[..copy_len], &mut response_buf)
        {
            Ok(len) => {
                resp_len = len;
                ccid_info!("CCID: XfrBlock OK resp_len={}", resp_len);
            }
            Err(_e) => {
                ccid_error!("CCID: XfrBlock failed (card timeout or protocol error)");
                self.send_slot_status(seq, COMMAND_STATUS_FAILED, ICC_STATUS_PRESENT_ACTIVE, 0xFF);
                return;
            }
        }

        let resp_len = resp_len.min(response_buf.len());

        self.tx_buffer[0] = RDR_TO_PC_DATABLOCK;
        self.tx_buffer[1..5].copy_from_slice(&(resp_len as u32).to_le_bytes());
        self.tx_buffer[5] = 0;
        self.tx_buffer[6] = seq;
        self.tx_buffer[7] = Self::build_status(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE);
        self.tx_buffer[8] = 0;
        self.tx_buffer[9] = 0;

        self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + resp_len]
            .copy_from_slice(&response_buf[..resp_len]);

        self.tx_len = CCID_HEADER_SIZE + resp_len;
        ccid_debug!("CCID: XfrBlock success, resp_len={}", resp_len);
    }

    fn handle_get_parameters(&mut self, seq: u8) {
        let p = &self.atr_params;
        if self.current_protocol == 1 {
            let params: [u8; 7] = [
                if p.has_ta1 { p.ta1 } else { 0x11 },
                (p.edc_type & 1) << 4,
                p.guard_time_n,
                p.bwi.wrapping_sub(1).min(0x0A),
                0x00,
                p.ifsc.min(254),
                0x00,
            ];
            self.tx_buffer[0] = RDR_TO_PC_PARAMETERS;
            self.tx_buffer[1..5].copy_from_slice(&(params.len() as u32).to_le_bytes());
            self.tx_buffer[5] = 0;
            self.tx_buffer[6] = seq;
            self.tx_buffer[7] = Self::build_status(COMMAND_STATUS_NO_ERROR, self.get_icc_status());
            self.tx_buffer[8] = 0;
            self.tx_buffer[9] = 0;
            self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + params.len()]
                .copy_from_slice(&params);
            self.tx_len = CCID_HEADER_SIZE + params.len();
        } else {
            let params: [u8; 5] = [
                if p.has_ta1 { p.ta1 } else { 0x11 },
                0x00,
                p.guard_time_n,
                p.bwi.wrapping_sub(1).min(0x0A),
                0x00,
            ];
            self.tx_buffer[0] = RDR_TO_PC_PARAMETERS;
            self.tx_buffer[1..5].copy_from_slice(&(params.len() as u32).to_le_bytes());
            self.tx_buffer[5] = 0;
            self.tx_buffer[6] = seq;
            self.tx_buffer[7] = Self::build_status(COMMAND_STATUS_NO_ERROR, self.get_icc_status());
            self.tx_buffer[8] = 0;
            self.tx_buffer[9] = 0;
            self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + params.len()]
                .copy_from_slice(&params);
            self.tx_len = CCID_HEADER_SIZE + params.len();
        }
    }

    fn handle_set_parameters(&mut self, seq: u8) {
        let data_len = u32::from_le_bytes([
            self.rx_buffer[1],
            self.rx_buffer[2],
            self.rx_buffer[3],
            self.rx_buffer[4],
        ]) as usize;

        let requested_protocol = match data_len {
            5 => 0,
            7 => 1,
            _ => {
                ccid_error!("CCID: SetParameters invalid dwLength={}", data_len);
                self.send_slot_status(seq, COMMAND_STATUS_FAILED, self.get_icc_status(), 0x07);
                return;
            }
        };

        self.driver.set_protocol(requested_protocol);
        self.current_protocol = requested_protocol;

        let p = &self.atr_params;
        if self.current_protocol == 1 {
            let params: [u8; 7] = [
                if p.has_ta1 { p.ta1 } else { 0x11 },
                (p.edc_type & 1) << 4,
                p.guard_time_n,
                p.bwi.wrapping_sub(1).min(0x0A),
                0x00,
                p.ifsc.min(254),
                0x00,
            ];
            self.tx_buffer[0] = RDR_TO_PC_PARAMETERS;
            self.tx_buffer[1..5].copy_from_slice(&7u32.to_le_bytes());
            self.tx_buffer[5] = 0;
            self.tx_buffer[6] = seq;
            self.tx_buffer[7] = Self::build_status(COMMAND_STATUS_NO_ERROR, self.get_icc_status());
            self.tx_buffer[8] = 0;
            self.tx_buffer[9] = 0;
            self.tx_buffer[10..17].copy_from_slice(&params);
            self.tx_len = CCID_HEADER_SIZE + 7;
        } else {
            let params: [u8; 5] = [
                if p.has_ta1 { p.ta1 } else { 0x11 },
                0x00,
                p.guard_time_n,
                p.bwi.wrapping_sub(1).min(0x0A),
                0x00,
            ];
            self.tx_buffer[0] = RDR_TO_PC_PARAMETERS;
            self.tx_buffer[1..5].copy_from_slice(&5u32.to_le_bytes());
            self.tx_buffer[5] = 0;
            self.tx_buffer[6] = seq;
            self.tx_buffer[7] = Self::build_status(COMMAND_STATUS_NO_ERROR, self.get_icc_status());
            self.tx_buffer[8] = 0;
            self.tx_buffer[9] = 0;
            self.tx_buffer[10..15].copy_from_slice(&params);
            self.tx_len = CCID_HEADER_SIZE + 5;
        }
    }

    fn handle_secure(&mut self, seq: u8) {
        if self.slot_state != SlotState::PresentActive {
            self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_SLOT_BUSY);
            return;
        }

        if self.rx_len <= 10 {
            self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            return;
        }

        let pin_operation = self.rx_buffer[10];

        match pin_operation {
            0x00 => {
                let pin_data = &self.rx_buffer[11..self.rx_len];

                match PinVerifyParams::parse(pin_data) {
                    Some(params) => {
                        self.secure_state = SecureState::WaitingForPinVerify { seq, params };
                        ccid_debug!("CCID: PIN Verify - waiting for PIN entry");
                    }
                    None => {
                        ccid_warn!("CCID: PIN Verify parse failed");
                        self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
                    }
                }
            }
            0x01 => {
                let pin_data = &self.rx_buffer[11..self.rx_len];

                match PinModifyParams::parse(pin_data) {
                    Some(params) => {
                        self.secure_state = SecureState::WaitingForPinModify { seq, params };
                        ccid_debug!("CCID: PIN Modify - waiting for PIN entry");
                    }
                    None => {
                        ccid_warn!("CCID: PIN Modify parse failed");
                        self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
                    }
                }
            }
            _ => {
                ccid_warn!("CCID: Unknown PIN operation: {}", pin_operation);
                self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
        }
    }

    fn handle_escape(&mut self, seq: u8) {
        let is_gemalto = self.vendor_id == 0x08E6;

        if !is_gemalto {
            ccid_debug!(
                "CCID: ESCAPE rejected for non-Gemalto vendor 0x{:04X}",
                self.vendor_id
            );
            self.send_err_resp(PC_TO_RDR_ESCAPE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            return;
        }

        let data_len = u32::from_le_bytes([
            self.rx_buffer[1],
            self.rx_buffer[2],
            self.rx_buffer[3],
            self.rx_buffer[4],
        ]) as usize;

        if data_len >= 1 && self.rx_buffer[CCID_HEADER_SIZE] == 0x6A {
            ccid_debug!("CCID: GET_FIRMWARE_FEATURES ESCAPE (0x6A)");
            let firmware_features: [u8; 15] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            let icc = self.get_icc_status();
            self.tx_buffer[0] = RDR_TO_PC_ESCAPE;
            self.tx_buffer[1..5].copy_from_slice(&(firmware_features.len() as u32).to_le_bytes());
            self.tx_buffer[5] = 0;
            self.tx_buffer[6] = seq;
            self.tx_buffer[7] = Self::build_status(COMMAND_STATUS_NO_ERROR, icc);
            self.tx_buffer[8] = 0;
            self.tx_buffer[9] = 0;
            self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + firmware_features.len()]
                .copy_from_slice(&firmware_features);
            self.tx_len = CCID_HEADER_SIZE + firmware_features.len();
        } else {
            ccid_debug!(
                "CCID: ESCAPE unknown command 0x{:02X}",
                self.rx_buffer[CCID_HEADER_SIZE]
            );
            self.send_err_resp(PC_TO_RDR_ESCAPE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
        }
    }

    pub fn is_pin_entry_active(&self) -> bool {
        matches!(
            self.secure_state,
            SecureState::WaitingForPinVerify { .. } | SecureState::WaitingForPinModify { .. }
        )
    }

    pub fn is_pin_verify_active(&self) -> bool {
        matches!(self.secure_state, SecureState::WaitingForPinVerify { .. })
    }

    pub fn is_pin_modify_active(&self) -> bool {
        matches!(self.secure_state, SecureState::WaitingForPinModify { .. })
    }

    pub fn take_secure_params(&mut self) -> Option<(u8, PinVerifyParams)> {
        if let SecureState::WaitingForPinVerify { seq, params } =
            core::mem::replace(&mut self.secure_state, SecureState::Idle)
        {
            Some((seq, params))
        } else {
            None
        }
    }

    pub fn take_secure_modify_params(&mut self) -> Option<(u8, PinModifyParams)> {
        if let SecureState::WaitingForPinModify { seq, params } =
            core::mem::replace(&mut self.secure_state, SecureState::Idle)
        {
            Some((seq, params))
        } else {
            None
        }
    }

    pub fn complete_pin_entry(&mut self, seq: u8, result: PinResult, apdu_response: Option<&[u8]>) {
        ccid_debug!(
            "CCID: PIN entry complete - seq={}, result={:?}",
            seq,
            result
        );

        let icc_status = self.get_icc_status();

        match result {
            PinResult::Success => {
                if let Some(resp) = apdu_response {
                    self.send_data_block_response(
                        seq,
                        resp,
                        COMMAND_STATUS_NO_ERROR,
                        icc_status,
                        0,
                    );
                } else {
                    self.send_data_block_response(seq, &[], COMMAND_STATUS_NO_ERROR, icc_status, 0);
                }
            }
            PinResult::Cancelled => {
                ccid_warn!("CCID: PIN entry cancelled by user");
                self.send_data_block_response(
                    seq,
                    &[],
                    COMMAND_STATUS_FAILED,
                    icc_status,
                    CCID_ERR_PIN_CANCELLED,
                );
            }
            PinResult::Timeout => {
                ccid_warn!("CCID: PIN entry timed out");
                self.send_data_block_response(
                    seq,
                    &[],
                    COMMAND_STATUS_FAILED,
                    icc_status,
                    CCID_ERR_PIN_TIMEOUT,
                );
            }
            PinResult::InvalidLength => {
                ccid_warn!("CCID: Invalid PIN length");
                self.send_data_block_response(
                    seq,
                    &[],
                    COMMAND_STATUS_FAILED,
                    icc_status,
                    CCID_ERR_CMD_ABORTED,
                );
            }
        }

        self.secure_state = SecureState::Idle;
    }

    pub fn is_card_present(&self) -> bool {
        self.driver.is_card_present()
    }

    pub fn is_card_active(&self) -> bool {
        self.slot_state == SlotState::PresentActive
    }

    #[cfg(feature = "display")]
    pub fn set_pin_result(
        &mut self,
        seq: u8,
        result: PinResult,
        buffer: PinBuffer,
        params: PinVerifyParams,
    ) {
        ccid_debug!(
            "CCID: Storing PIN result - seq={}, result={:?}, pin_len={}",
            seq,
            result,
            buffer.len()
        );
        self.pin_result_pending = Some((seq, result, buffer, params));
    }

    #[cfg(feature = "display")]
    pub fn process_pin_result(&mut self) {
        let Some((seq, result, buffer, params)) = self.pin_result_pending.take() else {
            return;
        };

        ccid_debug!(
            "CCID: Processing PIN result - seq={}, result={:?}, min_len={}, max_len={}",
            seq,
            result,
            params.min_len,
            params.max_len
        );

        match result {
            PinResult::Success => {
                let ascii_pin = buffer.to_ascii();
                let pin_len = buffer.len();

                ccid_debug!(
                    "CCID: Building APDU - CLA={:02X}, P1={:02X}, P2={:02X}, pin_len={}",
                    params.apdu_template[0],
                    params.apdu_template[2],
                    params.apdu_template[3],
                    pin_len
                );

                let builder = VerifyApduBuilder::from_template(
                    params.apdu_template[0],
                    params.apdu_template[2],
                    params.apdu_template[3],
                );

                match builder.build(&ascii_pin[..pin_len]) {
                    Ok(apdu) => {
                        let apdu_len = 5 + pin_len;
                        ccid_debug!("CCID: Transmitting APDU, len={}", apdu_len);
                        match self
                            .driver
                            .transmit_apdu(&apdu[..apdu_len], &mut self.response_buffer)
                        {
                            Ok(resp_len) => {
                                ccid_info!("CCID: Card responded, len={}", resp_len);
                                let mut resp_copy: [u8; 261] = [0u8; 261];
                                resp_copy[..resp_len]
                                    .copy_from_slice(&self.response_buffer[..resp_len]);
                                self.complete_pin_entry(
                                    seq,
                                    PinResult::Success,
                                    Some(&resp_copy[..resp_len]),
                                );
                            }
                            Err(_) => {
                                ccid_warn!("CCID: Card transmit failed");
                                self.complete_pin_entry(seq, PinResult::Cancelled, None);
                            }
                        }
                    }
                    Err(_e) => {
                        ccid_warn!("CCID: APDU build failed");
                        self.complete_pin_entry(seq, PinResult::InvalidLength, None);
                    }
                }
            }
            PinResult::Cancelled | PinResult::Timeout | PinResult::InvalidLength => {
                ccid_debug!("CCID: PIN entry failed with {:?}", result);
                self.complete_pin_entry(seq, result, None);
            }
        }
    }

    #[cfg(feature = "display")]
    pub fn set_pin_modify_result(
        &mut self,
        seq: u8,
        result: PinResult,
        old_buffer: PinBuffer,
        new_buffer: PinBuffer,
        params: PinModifyParams,
    ) {
        ccid_debug!(
            "CCID: Storing PIN modify result - seq={}, result={:?}, old_len={}, new_len={}",
            seq,
            result,
            old_buffer.len(),
            new_buffer.len()
        );
        self.pin_modify_result_pending = Some((seq, result, old_buffer, new_buffer, params));
    }

    #[cfg(feature = "display")]
    pub fn process_pin_modify_result(&mut self) {
        let Some((seq, result, old_buffer, new_buffer, params)) =
            self.pin_modify_result_pending.take()
        else {
            return;
        };

        ccid_debug!(
            "CCID: Processing PIN modify result - seq={}, result={:?}, min_len={}, max_len={}",
            seq,
            result,
            params.min_len,
            params.max_len
        );

        match result {
            PinResult::Success => {
                let old_pin = old_buffer.to_ascii();
                let old_len = old_buffer.len();
                let new_pin = new_buffer.to_ascii();
                let new_len = new_buffer.len();

                ccid_debug!(
                    "CCID: Building CHANGE REFERENCE DATA APDU - CLA={:02X}, P1={:02X}, P2={:02X}, old_len={}, new_len={}",
                    params.apdu_template[0],
                    params.apdu_template[2],
                    params.apdu_template[3],
                    old_len,
                    new_len
                );

                let builder = ModifyApduBuilder::from_template(
                    params.apdu_template[0],
                    params.apdu_template[2],
                    params.apdu_template[3],
                    params.old_pin_offset as usize,
                    params.new_pin_offset as usize,
                );

                match builder.build(&old_pin[..old_len], &new_pin[..new_len]) {
                    Ok(apdu) => {
                        let apdu_len = 5 + old_len + new_len;
                        ccid_debug!("CCID: Transmitting CHANGE APDU, len={}", apdu_len);
                        match self
                            .driver
                            .transmit_apdu(&apdu[..apdu_len], &mut self.response_buffer)
                        {
                            Ok(resp_len) => {
                                ccid_info!("CCID: Card responded to CHANGE, len={}", resp_len);
                                let mut resp_copy: [u8; 261] = [0u8; 261];
                                resp_copy[..resp_len]
                                    .copy_from_slice(&self.response_buffer[..resp_len]);
                                self.complete_pin_entry(
                                    seq,
                                    PinResult::Success,
                                    Some(&resp_copy[..resp_len]),
                                );
                            }
                            Err(_) => {
                                ccid_warn!("CCID: Card transmit failed for CHANGE");
                                self.complete_pin_entry(seq, PinResult::Cancelled, None);
                            }
                        }
                    }
                    Err(_e) => {
                        ccid_warn!("CCID: CHANGE APDU build failed");
                        self.complete_pin_entry(seq, PinResult::InvalidLength, None);
                    }
                }
            }
            PinResult::Cancelled | PinResult::Timeout | PinResult::InvalidLength => {
                ccid_debug!("CCID: PIN modify entry failed with {:?}", result);
                self.complete_pin_entry(seq, result, None);
            }
        }
    }

    fn send_data_block_response(
        &mut self,
        seq: u8,
        data: &[u8],
        cmd_status: u8,
        icc_status: u8,
        error: u8,
    ) {
        let data_len = data.len() as u32;

        self.tx_buffer[0] = RDR_TO_PC_DATABLOCK;
        self.tx_buffer[1..5].copy_from_slice(&data_len.to_le_bytes());
        self.tx_buffer[5] = 0;
        self.tx_buffer[6] = seq;
        self.tx_buffer[7] = Self::build_status(cmd_status, icc_status);
        self.tx_buffer[8] = error;
        self.tx_buffer[9] = 0;

        let header_len = CCID_HEADER_SIZE;
        if data.len() + header_len <= self.tx_buffer.len() {
            self.tx_buffer[header_len..header_len + data.len()].copy_from_slice(data);
            self.tx_len = header_len + data.len();
        } else {
            ccid_warn!("CCID: DataBlock response truncated");
            let truncated_len = self.tx_buffer.len() - header_len;
            self.tx_buffer[header_len..].copy_from_slice(&data[..truncated_len]);
            self.tx_len = self.tx_buffer.len();
        }

        ccid_trace!(
            "CCID: Sending DataBlock seq={}, len={}, status={}, error={}",
            seq,
            self.tx_len,
            self.tx_buffer[7],
            error
        );
    }

    fn send_slot_status(&mut self, seq: u8, cmd_status: u8, icc_status: u8, error: u8) {
        self.send_slot_status_with_clock(seq, cmd_status, icc_status, error, 0);
    }

    fn send_slot_status_with_clock(
        &mut self,
        seq: u8,
        cmd_status: u8,
        icc_status: u8,
        error: u8,
        b_clock_status: u8,
    ) {
        self.tx_buffer[0] = RDR_TO_PC_SLOTSTATUS;
        self.tx_buffer[1..5].copy_from_slice(&0u32.to_le_bytes());
        self.tx_buffer[5] = 0;
        self.tx_buffer[6] = seq;
        self.tx_buffer[7] = Self::build_status(cmd_status, icc_status);
        self.tx_buffer[8] = error;
        self.tx_buffer[9] = b_clock_status;

        self.tx_len = CCID_HEADER_SIZE;
    }

    pub fn set_card_present(&mut self, present: bool) {
        self.card_present_last = present;
        if present {
            self.slot_state = SlotState::PresentInactive;
        } else {
            self.slot_state = SlotState::Absent;
        }
    }

    pub fn set_rx_data(&mut self, data: &[u8]) {
        self.rx_buffer[..data.len()].copy_from_slice(data);
        self.rx_len = data.len();
    }

    pub fn get_tx_buffer(&self) -> &[u8] {
        &self.tx_buffer[..self.tx_len]
    }

    pub fn get_tx_len(&self) -> usize {
        self.tx_len
    }

    pub fn slot_state(&self) -> SlotState {
        self.slot_state
    }

    pub fn current_protocol(&self) -> u8 {
        self.current_protocol
    }

    pub fn cmd_busy(&self) -> bool {
        self.cmd_busy
    }
}
