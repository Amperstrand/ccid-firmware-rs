//! Thin STM32 wrapper over the shared `ccid-core` handler (DRY phase 2,
//! issue #121 / PERF_AND_DRY_STRATEGY §3.1).
//!
//! Command dispatch, slot lifecycle, bStatus assembly, the contact
//! parameter model (ATR-derived + host-supplied echo with libccid quirk
//! validation), the XfrBlock pipeline and the PC_to_RDR_Secure PIN
//! surface live in `ccid_core::handler::CcidHandlerCore<ContactProfile>`;
//! this module keeps what is genuinely firmware-local:
//!
//! - the USB bulk framing state (feed/message_ready/take_response,
//!   single-slot + cmd-busy gating)
//! - the vendor/transport escape set (0xD0 driver diagnostics, Gemalto
//!   0x6A firmware features)
//! - the smartcard-USART specifics (IccClock, SetDataRateAndClockFrequency)
//! - the display-feature PIN-result → VERIFY/CHANGE APDU exchange
//!
//! Wire behavior is byte-identical to the pre-port `CcidMessageHandler`
//! (firmware host suites + ccid-core contact-profile tests pin it).

use crate::driver::SmartcardDriver;
use crate::pinpad::{ModifyApduBuilder, PinBuffer, PinResult, PinVerifyParams, VerifyApduBuilder};
use ::ccid_core::handler::{CcidHandlerCore, ContactProfile};
use ::ccid_core::response::{write_message, write_slot_status};
use card_interface::{CardBackend, PresenceState, SlotDriver};
use ccid_protocol::status::build_bstatus;
pub use ccid_protocol::types::*;

/// Cherry SmartTerminal ST-2xxx vendor id — the only device profile with a
/// PIN pad (bPINSupport != 0). device_profile is ARM-only, so the handler
/// gates PC_to_RDR_Secure on the wired vendor id, mirroring the
/// is_gemalto Escape check.
///
/// 046A = Cherry GmbH (reference/CCID/readers/CherrySmartTerminalST2XXX.txt).
const PIN_PAD_VENDOR_ID: u16 = 0x046A;

/// CardBackend/SlotDriver adapter over the firmware's SmartcardDriver:
/// the core's slot contract (presence = card-detect GPIO, no session
/// short-circuit) with an optional test-only presence override.
struct SmartcardCoreAdapter<D: SmartcardDriver> {
    driver: D,
    forced_presence: Option<bool>,
}

impl<D: SmartcardDriver> CardBackend for SmartcardCoreAdapter<D> {
    type Error = D::Error;

    fn power_on(&mut self, atr_buf: &mut [u8]) -> core::result::Result<usize, Self::Error> {
        let atr = self.driver.power_on()?;
        let len = atr.len().min(atr_buf.len());
        atr_buf[..len].copy_from_slice(&atr[..len]);
        Ok(len)
    }

    fn power_off(&mut self) {
        self.driver.power_off();
    }

    fn is_card_present(&mut self) -> bool {
        match self.forced_presence {
            Some(p) => p,
            None => self.driver.is_card_present(),
        }
    }

    fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error> {
        self.driver.transmit_apdu(command, response)
    }
}

impl<D: SmartcardDriver> SlotDriver for SmartcardCoreAdapter<D> {
    fn poll_card_presence(&mut self) -> PresenceState {
        PresenceState {
            present: self.is_card_present(),
        }
    }

    fn session_active(&self) -> bool {
        false
    }
}

pub struct CcidMessageHandler<D: SmartcardDriver> {
    core: CcidHandlerCore<SmartcardCoreAdapter<D>, ContactProfile>,
    rx_buffer: [u8; MAX_CCID_MESSAGE_LENGTH],
    rx_len: usize,
    tx_buffer: [u8; MAX_CCID_MESSAGE_LENGTH],
    tx_len: usize,
    cmd_busy: bool,
    vendor_id: u16,
    response_buffer: [u8; 261],
    #[cfg(feature = "display")]
    pin_result_pending: Option<(u8, PinResult, PinBuffer, PinVerifyParams)>,
    #[cfg(feature = "display")]
    pin_modify_result_pending: Option<(
        u8,
        PinResult,
        PinBuffer,
        PinBuffer,
        crate::pinpad::PinModifyParams,
    )>,
}

impl<D: SmartcardDriver> CcidMessageHandler<D> {
    pub fn new(driver: D, vendor_id: u16) -> Self {
        let present = driver.is_card_present();
        ccid_info!("CCID init: card_present={}", present);
        let mut this = Self {
            core: CcidHandlerCore::with_profile(
                SmartcardCoreAdapter {
                    driver,
                    forced_presence: None,
                },
                b"",
                vendor_id == PIN_PAD_VENDOR_ID,
            ),
            rx_buffer: [0u8; MAX_CCID_MESSAGE_LENGTH],
            rx_len: 0,
            tx_buffer: [0u8; MAX_CCID_MESSAGE_LENGTH],
            tx_len: 0,
            cmd_busy: false,
            vendor_id,
            response_buffer: [0u8; 261],
            #[cfg(feature = "display")]
            pin_result_pending: None,
            #[cfg(feature = "display")]
            pin_modify_result_pending: None,
        };
        // Construction-time presence sync (the pre-port handler sampled
        // is_card_present() in new()): a card in the slot starts
        // PresentInactive without a change notification.
        let _ = this.core.check_card_change();
        this
    }

    pub fn driver(&self) -> &D {
        &self.core.driver().driver
    }

    pub fn driver_mut(&mut self) -> &mut D {
        &mut self.core.driver_mut().driver
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
            PC_TO_RDR_ICC_CLOCK => self.handle_icc_clock(seq),
            PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ => {
                self.handle_set_data_rate_and_clock(seq);
            }
            PC_TO_RDR_ESCAPE => self.handle_escape(seq),
            PC_TO_RDR_XFR_BLOCK if self.declared_length() > 261 => {
                // Oversize (Extended APDU) rejection happens on the
                // DECLARED dwLength, before the core's frame validation —
                // the pre-port byte shape (pinned by the firmware suite).
                ccid_warn!("CCID: XfrBlock Extended APDU rejected");
                self.send_slot_status(seq, COMMAND_STATUS_FAILED, ICC_STATUS_PRESENT_ACTIVE, 0x07);
            }
            PC_TO_RDR_T0_APDU => {
                ccid_debug!("CCID: T0APDU command (stub - TPDU level sufficient)");
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
            PC_TO_RDR_MECHANICAL => {
                ccid_debug!("CCID: Mechanical command (stub - no mechanical parts)");
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
            _ => self.delegate_to_core(),
        }

        // The accepted-protocol event (the esp32 t1-armed pattern):
        // SetParameters/ResetParameters acceptance re-programs the
        // smartcard USART for the negotiated protocol.
        if self.core.take_parameters_accepted() {
            let protocol = self.core.current_protocol();
            self.core.driver_mut().driver.set_protocol(protocol);
        }
    }

    fn delegate_to_core(&mut self) {
        self.tx_len = self
            .core
            .process_command(&self.rx_buffer[..self.rx_len], &mut self.tx_buffer);
        if self.tx_len == 0 {
            // Deferred response (PC_to_RDR_Secure PIN entry): no bytes
            // go out until complete_pin_entry writes the DataBlock.
            ccid_debug!("CCID: response deferred (PIN entry)");
        }
    }

    fn declared_length(&self) -> usize {
        u32::from_le_bytes([
            self.rx_buffer[1],
            self.rx_buffer[2],
            self.rx_buffer[3],
            self.rx_buffer[4],
        ]) as usize
    }

    pub fn take_response(&mut self) -> (usize, &[u8]) {
        let len = self.tx_len;
        self.rx_len = 0;
        self.tx_len = 0;
        self.cmd_busy = false;
        (len, &self.tx_buffer[..len])
    }

    pub fn check_card_presence(&mut self) -> Option<bool> {
        let changed = self.core.check_card_change().map(|_| true);
        if changed.is_some() && !self.core.presence_state().present {
            ccid_info!("Card removed, powering off driver");
            self.rx_len = 0;
            self.cmd_busy = false;
            #[cfg(feature = "display")]
            {
                self.pin_result_pending = None;
                self.pin_modify_result_pending = None;
            }
        }
        changed
    }

    pub fn get_icc_status(&self) -> u8 {
        self.core.slot_state().icc_status()
    }

    /// bmSlotCCState per CCID Rev 1.1 §6.3.1: bit 2n = slot n card present,
    /// bit 2n+1 = change since last notification (single slot: bits 0/1).
    /// The serial twin of this encoding is CARD_PRESENT=0x03 /
    /// CARD_ABSENT=0x02 (present/absent + changed) in ccid-transport-serial.
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

    fn send_err_resp(&mut self, msg_type: u8, seq: u8, error: u8) {
        let icc = self.get_icc_status();
        let status = build_bstatus(COMMAND_STATUS_FAILED, icc);
        match msg_type {
            PC_TO_RDR_ESCAPE => {
                self.tx_len = write_message(
                    RDR_TO_PC_ESCAPE,
                    0,
                    seq,
                    status,
                    error,
                    0,
                    &[],
                    &mut self.tx_buffer,
                );
            }
            _ => {
                self.send_slot_status(seq, COMMAND_STATUS_FAILED, icc, error);
            }
        }
    }

    // CCID_SPEC: /* Section 6.1.14 */ struct ccid_pc_to_rdr_set_rate_and_clock {
    // struct ccid_header hdr; uint8_t abRFU[3]; uint32_t dwClockFrequency;
    // uint32_t dwDataRate; } __attribute__ ((packed));
    // /* Response: RDR_to_PC_DataRateAndClockFrequency */
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
        match self
            .core
            .driver_mut()
            .driver
            .set_clock_and_rate(clock_hz, rate_bps)
        {
            Ok((actual_clock, actual_rate)) => {
                let payload = [
                    actual_clock.to_le_bytes()[0],
                    actual_clock.to_le_bytes()[1],
                    actual_clock.to_le_bytes()[2],
                    actual_clock.to_le_bytes()[3],
                    actual_rate.to_le_bytes()[0],
                    actual_rate.to_le_bytes()[1],
                    actual_rate.to_le_bytes()[2],
                    actual_rate.to_le_bytes()[3],
                ];
                self.tx_len = write_message(
                    RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ,
                    0,
                    seq,
                    build_bstatus(COMMAND_STATUS_NO_ERROR, self.get_icc_status()),
                    0,
                    0,
                    &payload,
                    &mut self.tx_buffer,
                );
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

    // CCID_SPEC: /* Section 6.1.9 */ enum ccid_clock_command {
    // CCID_CLOCK_CMD_RESTART = 0x00, CCID_CLOCK_CMD_STOP = 0x01, };
    // struct ccid_pc_to_rdr_icc_clock { struct ccid_header hdr;
    // uint8_t bClockCommand; uint8_t abRFU[2]; } __attribute__ ((packed));
    // /* response: RDR_to_PC_SlotStatus */
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
        self.core.driver_mut().driver.set_clock(enable);
        let b_clock_status: u8 = if enable { 0x00 } else { 0x01 };
        self.send_slot_status_with_clock(seq, COMMAND_STATUS_NO_ERROR, icc, 0, b_clock_status);
    }

    // CCID_SPEC: /* Section 6.1.8 */ struct ccid_pc_to_rdr_escape {
    // struct ccid_header hdr; uint8_t abRFU[3]; uint8_t abData[0]; }
    // __attribute__ ((packed)); /* Response: RDR_to_PC_Escape */
    fn handle_escape(&mut self, seq: u8) {
        let data_len = self.declared_length();

        if data_len >= 1 && self.rx_buffer[CCID_HEADER_SIZE] == 0xD0 {
            ccid_debug!("CCID: DIAGNOSTICS ESCAPE (0xD0)");
            let diag = self.driver().diagnostics();
            let mut diag_buf = [0u8; ::ccid_core::Diagnostics::SERIALIZED_SIZE];
            diag.to_bytes(&mut diag_buf);
            self.tx_len = write_message(
                RDR_TO_PC_ESCAPE,
                0,
                seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, self.get_icc_status()),
                0,
                0,
                &diag_buf,
                &mut self.tx_buffer,
            );
            return;
        }

        let is_gemalto = self.vendor_id == 0x08E6;

        if !is_gemalto {
            ccid_debug!(
                "CCID: ESCAPE rejected for non-Gemalto vendor 0x{:04X}",
                self.vendor_id
            );
            self.send_err_resp(PC_TO_RDR_ESCAPE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            return;
        }

        if data_len >= 1 && self.rx_buffer[CCID_HEADER_SIZE] == 0x6A {
            ccid_debug!("CCID: GET_FIRMWARE_FEATURES ESCAPE (0x6A)");
            // Gemalto IDBridge firmware-features convention (b0 = 'vendor
            // feature support'; readers/Gemalto_IDBridge_CT30.txt family):
            // a 15-byte TLV-style capability bitmap, all other bits clear.
            let firmware_features: [u8; 15] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            self.tx_len = write_message(
                RDR_TO_PC_ESCAPE,
                0,
                seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, self.get_icc_status()),
                0,
                0,
                &firmware_features,
                &mut self.tx_buffer,
            );
        } else {
            ccid_debug!(
                "CCID: ESCAPE unknown command 0x{:02X}",
                self.rx_buffer[CCID_HEADER_SIZE]
            );
            self.send_err_resp(PC_TO_RDR_ESCAPE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
        }
    }

    pub fn is_pin_entry_active(&self) -> bool {
        self.core.is_pin_entry_active()
    }

    pub fn is_pin_verify_active(&self) -> bool {
        self.core.is_pin_verify_active()
    }

    pub fn is_pin_modify_active(&self) -> bool {
        self.core.is_pin_modify_active()
    }

    pub fn take_secure_params(&mut self) -> Option<(u8, crate::pinpad::PinVerifyParams)> {
        self.core.take_secure_params()
    }

    pub fn take_secure_modify_params(&mut self) -> Option<(u8, crate::pinpad::PinModifyParams)> {
        self.core.take_secure_modify_params()
    }

    pub fn complete_pin_entry(&mut self, seq: u8, result: PinResult, apdu_response: Option<&[u8]>) {
        ccid_debug!(
            "CCID: PIN entry complete - seq={}, result={}",
            seq,
            result.as_str()
        );
        self.tx_len = self
            .core
            .complete_pin_entry(seq, result, apdu_response, &mut self.tx_buffer);
    }

    pub fn is_card_present(&self) -> bool {
        self.core
            .driver()
            .forced_presence
            .unwrap_or_else(|| self.driver().is_card_present())
    }

    pub fn is_card_active(&self) -> bool {
        self.core.slot_state() == SlotState::PresentActive
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
            "CCID: Storing PIN result - seq={}, result={}, pin_len={}",
            seq,
            result.as_str(),
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
            "CCID: Processing PIN result - seq={}, result={}, min_len={}, max_len={}",
            seq,
            result.as_str(),
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
                            .core
                            .driver_mut()
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
                ccid_debug!("CCID: PIN entry failed with {}", result.as_str());
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
        params: crate::pinpad::PinModifyParams,
    ) {
        ccid_debug!(
            "CCID: Storing PIN modify result - seq={}, result={}, old_len={}, new_len={}",
            seq,
            result.as_str(),
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
            "CCID: Processing PIN modify result - seq={}, result={}, min_len={}, max_len={}",
            seq,
            result.as_str(),
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
                            .core
                            .driver_mut()
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
                ccid_debug!("CCID: PIN modify entry failed with {}", result.as_str());
                self.complete_pin_entry(seq, result, None);
            }
        }
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
        self.tx_len = write_slot_status(
            0,
            seq,
            icc_status,
            cmd_status,
            error,
            b_clock_status,
            &mut self.tx_buffer,
        );
    }

    pub fn set_card_present(&mut self, present: bool) {
        // Test/harness hook: force the presence the adapter reports, then
        // resync the core slot state through the normal change path.
        self.core.driver_mut().forced_presence = Some(present);
        let _ = self.core.check_card_change();
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
        self.core.slot_state()
    }

    pub fn current_protocol(&self) -> u8 {
        self.core.current_protocol()
    }

    pub fn cmd_busy(&self) -> bool {
        self.cmd_busy
    }
}
