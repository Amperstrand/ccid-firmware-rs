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

/// Core clock driving DWT CYCCNT on this build — the time base for the
/// 0xD0 `uptime_ticks` diagnostic (#268 defect 2). F469: 168 MHz sysclk,
/// F746: 216 MHz (main.rs RCC configs).
#[cfg(feature = "stm32f469")]
pub const UPTIME_CLOCK_HZ: u32 = 168_000_000;
#[cfg(all(feature = "stm32f746", not(feature = "stm32f469")))]
pub const UPTIME_CLOCK_HZ: u32 = 216_000_000;
#[cfg(not(any(feature = "stm32f469", feature = "stm32f746")))]
pub const UPTIME_CLOCK_HZ: u32 = 1_000;

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

    fn xfr_error_class(&self, err: &Self::Error) -> card_interface::XfrErrorKind {
        // Forward to the concrete frontend's classification (#136):
        // SmartcardWrapper matches on SmartcardError so parity reaches
        // the core's error-map table as XfrErrorKind::Parity (→ CCID
        // bError 0xFD).
        self.driver.xfr_error_class(err)
    }

    fn reinit_count(&self) -> u32 {
        self.driver.reinit_count()
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
    uptime: crate::dwt_watchdog::DwtUptime,
    pin_result_pending: Option<(u8, PinResult, PinBuffer, PinVerifyParams)>,
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
            uptime: crate::dwt_watchdog::DwtUptime::new(UPTIME_CLOCK_HZ),
            pin_result_pending: None,
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

    /// #290: forward the wrapper-driven card-transmit seam — the
    /// app-enumeration display lane drives real SELECT APDUs and
    /// belongs in the 0xD0 counters with the PIN-pad arms.
    pub fn transmit_counted(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> Result<usize, D::Error> {
        self.core.transmit_counted(command, response)
    }

    /// Escape 0xD0 counters snapshot (the esp32 wrapper's accessor
    /// pattern) — also the #276 regression-test surface: PIN-pad
    /// exchanges must advance apdu_tx/apdu_rx like XfrBlock traffic.
    pub fn diagnostics(&self) -> &ccid_core::Diagnostics {
        self.core.diagnostics()
    }

    /// Advance the 0xD0 diagnostics time base and resync the core's
    /// snapshot (uptime + driver reinit count). The USB transport calls
    /// this once per poll with the current DWT CYCCNT reading; host
    /// tests inject deterministic cycle counts (#268 defect 2 — the
    /// HIL observed uptime=0 forever because nothing ever pumped it).
    pub fn poll_diagnostics(&mut self, now_cycles: u32) {
        let ticks = self.uptime.poll(now_cycles);
        self.core.refresh_diagnostics(ticks);
    }

    pub fn feed(&mut self, data: &[u8]) {
        let remaining = self.rx_buffer.len() - self.rx_len;
        let copy_len = data.len().min(remaining);
        self.rx_buffer[self.rx_len..self.rx_len + copy_len].copy_from_slice(&data[..copy_len]);
        self.rx_len += copy_len;
        self.drop_unassemblable();
    }

    /// #268 defect 3 (HIL Leg 3 wedge): once a received header declares
    /// more payload than the reassembly buffer can ever hold,
    /// `message_ready()` can never become true again — every later
    /// command (GetSlotStatus included) went unanswered across host
    /// reconnects and USB port resets; only an MCU reset cleared it on
    /// the bench. Drop the partial buffer so the stream resynchronizes
    /// on the next packet — the serial transport's corrupt-frame
    /// recovery doctrine (ccid_serial_server garbage tolerance).
    /// Messages that CAN complete (total ≤ buffer size) are untouched.
    fn drop_unassemblable(&mut self) {
        if self.rx_len >= CCID_HEADER_SIZE
            && !declared_fits(self.declared_length(), self.rx_buffer.len())
        {
            ccid_warn!("CCID: rx resync (declared dwLength un-assemblable)");
            self.rx_len = 0;
        }
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
        // Wrap guard (Codex P1 on #269): without it, `CCID_HEADER_SIZE +
        // msg_len` wraps on the 32-bit MCU target whenever dwLength sits
        // within CCID_HEADER_SIZE of u32::MAX — the wrapped sum then
        // passes as a small total, message_ready() fires on a partial
        // buffer, and handle_message slices garbage (panic on target).
        if !declared_fits(msg_len, self.rx_buffer.len()) {
            return false;
        }
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
        // smartcard USART for the negotiated protocol — and, #122, the
        // accepted wire timing (Fi/Di + guard time N, the dwFeatures
        // bit-5 AUTO_BAUD contract; the driver guards the baud against
        // the card's actually-negotiated state, guard time is always
        // safe to apply).
        if self.core.take_parameters_accepted() {
            let protocol = self.core.current_protocol();
            self.core.driver_mut().driver.set_protocol(protocol);
            if let Some(timing) = self.core.accepted_contact_timing() {
                self.core.driver_mut().driver.set_wire_timing(
                    timing.fi,
                    timing.di,
                    timing.guard_time_n,
                );
            }
        }

        // #342: surface driver self-healing re-inits inline. A re-init
        // tore down and re-powered the card MID-SESSION — the session
        // state both sides hold is invalidated (ATR + negotiated
        // parameters on ours, T=1 link state on the host's). Consume
        // the driver's latch so the CCID layer knows it happened: the
        // 0xD0 reinit word refreshes immediately instead of waiting
        // for the next presence poll (check_card_change). Wire-clean
        // by construction — this is the pull-diagnostics doctrine, no
        // unsolicited bytes ever leave the reader.
        if self.core.driver_mut().driver.take_reinit_pending() {
            ccid_warn!("CCID: driver self-healing re-init fired (card session reset mid-session)");
            let count = self.core.driver_mut().driver.reinit_count();
            self.core.diagnostics_mut().reinit_count = count;
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
            self.pin_result_pending = None;
            self.pin_modify_result_pending = None;
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
            // Live counters from the shared core handler (apdu_tx/rx are
            // counted there around every card exchange) — #268 defect 2:
            // this used to read the SmartcardDriver trait stub, which on
            // the F469 only filled reinit_count + card_present, so the
            // HIL saw apdu_tx=0/apdu_rx=0/uptime=0 after real exchanges.
            let mut diag = *self.core.diagnostics();
            // #344: driver-known state overlays the core's counters —
            // the F746 bit-bang wrapper stamps its live presence-pin
            // card_present (the cached snapshot only refreshes on a
            // presence poll). The default trait fold is a no-op, so
            // F469/mock behavior is byte-identical.
            self.driver_mut().diagnostics(&mut diag);
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
            // libccid accepts the firmware-features answer ONLY when its
            // length is exactly sizeof(struct GEMALTO_FIRMWARE_FEATURES)
            // (reference/CCID/src/ccid.c set_gemalto_firmware_features:
            // "len_features == sizeof *gf_features"); any other length is
            // silently discarded. 21 bytes: no LCD, no PIN features, no
            // firewall — consistent with bPINSupport=0x00 / wLcdLayout
            // =0x0000 in the CT30/K30 profiles. Field map below follows
            // the struct declaration:
            // CCID_DEFS: gemalto_firmware_features; ... struct GEMALTO_FIRMWARE_FEATURES
            // CCID_DEFS: ...{
            // CCID_DEFS: ...unsigned char bLogicalLCDLineNumber; /* Logical number of LCD lines */
            // CCID_DEFS: ...unsigned char bLogicalLCDRowNumber; /* Logical number of characters per LCD line */
            // CCID_DEFS: ...unsigned char bLcdInfo; /* b0 indicates if scrolling is available */
            // CCID_DEFS: ...unsigned char bEntryValidationCondition; /* See PIN_PROPERTIES */
            let firmware_features: [u8; 21] = [
                0, // bLogicalLCDLineNumber
                0, // bLogicalLCDRowNumber
                0, // bLcdInfo
                0, // bEntryValidationCondition
                0, // VerifyPinStart..Abort bitfield byte
                0, // GetKey/WriteDisplay/SetSpeMessage/RFU bitfield byte
                0, 0, // RFUb2
                0, // bTimeOut2..RFUb3 bitfield byte
                0, 0, 0, // RFUb4
                1, // VersionNumber
                0, // MinimumPINSize
                0, // MaximumPINSize
                0, // Firewall/RFUb5 bitfield byte
                0, 0, // FirewalledCommand_SW1/SW2
                0, 0, 0, // RFUb6
            ];
            // struct tail at offset 12 (after the Part-10 feature bitfield
            // bytes, RFUb2[2], the bTimeOut2 byte and RFUb4[3]):
            // CCID_DEFS: unsigned char VersionNumber; /* ?? */
            // CCID_DEFS: ...unsigned char MinimumPINSize; /* for Verify and Modify */
            // CCID_DEFS: ...unsigned char MaximumPINSize;
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
                            .transmit_counted(&apdu[..apdu_len], &mut self.response_buffer)
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
                            .transmit_counted(&apdu[..apdu_len], &mut self.response_buffer)
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

/// Wrap-safe payload-capacity check (Codex P1 on #269): the naive
/// `CCID_HEADER_SIZE + declared > buffer_len` ADDS first — on the
/// 32-bit MCU target (usize == u32) any dwLength within
/// CCID_HEADER_SIZE of u32::MAX wraps the sum down to a small number,
/// defeating BOTH un-assemblable guards at once (drop_unassemblable
/// keeps the wedge, message_ready fires on a partial buffer). Subtract
/// instead: the buffer is always at least a header, so the subtraction
/// cannot underflow, and `declared` compares against the true payload
/// capacity. Total (width-independent): small `declared` fits, header
/// sized and above does not.
const fn declared_fits(declared: usize, buffer_len: usize) -> bool {
    buffer_len >= CCID_HEADER_SIZE && declared <= buffer_len - CCID_HEADER_SIZE
}

#[cfg(test)]
mod wrap_tests {
    use super::*;

    // Width-independent: these declared values wrap `10 + declared` on
    // ANY usize width that can hold them — the host (64-bit) proves the
    // arithmetic form, the target (32-bit) hits the same path with real
    // dwLength bytes 0xFFFF_FFF7..=0xFFFF_FFFF.
    #[test]
    fn usize_max_neighborhood_never_fits() {
        assert!(!declared_fits(usize::MAX, 271));
        assert!(!declared_fits(usize::MAX - CCID_HEADER_SIZE + 1, 271));
        assert!(!declared_fits(usize::MAX - CCID_HEADER_SIZE, 271));
    }

    #[test]
    fn buffer_smaller_than_header_never_fits() {
        // Totality guard: a hypothetical buffer shorter than a header
        // must not underflow the subtraction into a false "fits".
        assert!(!declared_fits(0, CCID_HEADER_SIZE - 1));
        assert!(!declared_fits(usize::MAX, CCID_HEADER_SIZE - 1));
    }

    #[test]
    fn true_capacity_boundary_is_exact() {
        assert!(declared_fits(261, 271)); // exactly buffer - header
        assert!(!declared_fits(262, 271)); // one past capacity
        assert!(declared_fits(0, 271));
    }
}

#[cfg(test)]
mod escape_diag_fold_tests {
    //! #344: the Escape 0xD0 answer is the core's LIVE counters
    //! OVERLAID with driver-known state. The F746 bit-bang wrapper
    //! (arm-only, not host-testable) folds its live presence-pin
    //! reading; these tests lock the plumbing with the mock's
    //! equivalent seam — the wire answer must carry the driver's
    //! card_present between presence polls, keep the core-owned
    //! counters intact (the #268 lesson, inverted), and stay
    //! byte-identical for default-fold drivers.

    use super::*;

    const CHERRY: u16 = 0x046A;

    fn frame(msg_type: u8, seq: u8, payload: &[u8]) -> std::vec::Vec<u8> {
        let mut f = std::vec::Vec::with_capacity(10 + payload.len());
        f.push(msg_type);
        f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        f.push(0); // slot
        f.push(seq);
        f.extend_from_slice(&[0, 0, 0]); // bSpecific(3)
        f.extend_from_slice(payload);
        f
    }

    fn escape_d0(
        h: &mut CcidMessageHandler<crate::mock_driver::MockSmartcardDriver>,
    ) -> ccid_core::Diagnostics {
        h.set_rx_data(&frame(PC_TO_RDR_ESCAPE, 0x42, &[0xD0]));
        h.handle_message();
        let (len, resp) = h.take_response();
        assert_eq!(resp[0], RDR_TO_PC_ESCAPE, "0xD0 answers an Escape");
        assert_eq!(
            len,
            10 + ccid_core::Diagnostics::SERIALIZED_SIZE,
            "payload is the frozen 28-byte layout"
        );
        ccid_core::Diagnostics::from_bytes(&resp[10..len])
            .expect("28-byte diagnostics payload decodes")
    }

    #[test]
    fn driver_fold_backs_card_present_between_presence_polls() {
        // the polled cache said ABSENT at construction; the driver's
        // live view (the F746 presence pin) says a card IS in — the
        // 0xD0 answer must carry the driver's reading, not the stale
        // cache and not the fabricated all-zero default
        let mut h = CcidMessageHandler::new(
            crate::mock_driver::MockSmartcardDriver::new().with_diag_card_present(true),
            CHERRY,
        );
        assert!(!h.diagnostics().card_present, "cache starts absent");
        let diag = escape_d0(&mut h);
        assert!(
            diag.card_present,
            "the driver fold must back card_present in the answer"
        );
    }

    #[test]
    fn driver_fold_preserves_core_counters() {
        // one card exchange advances the core's apdu counters; the
        // driver fold must ADD its state, never reset the counters
        // (the #268 failure mode: a driver-side stub fabricating
        // zeros for core-owned fields)
        let mut h = CcidMessageHandler::new(
            crate::mock_driver::MockSmartcardDriver::new()
                .card_present(true)
                .with_atr(&[0x3B, 0x00])
                .with_apdu_response(&[0x90, 0x00])
                .with_diag_card_present(true),
            CHERRY,
        );
        h.set_rx_data(&frame(PC_TO_RDR_ICC_POWER_ON, 1, &[]));
        h.handle_message();
        let _ = h.take_response();
        h.set_rx_data(&frame(
            PC_TO_RDR_XFR_BLOCK,
            2,
            &[0x00, 0xC0, 0x00, 0x00, 0x00],
        ));
        h.handle_message();
        let _ = h.take_response();
        assert_eq!(h.diagnostics().apdu_tx_count, 1);

        let diag = escape_d0(&mut h);
        assert_eq!(diag.apdu_tx_count, 1, "core counters survive the fold");
        assert_eq!(diag.apdu_rx_count, 1);
        assert!(diag.card_present);
    }

    #[test]
    fn default_fold_keeps_cached_presence() {
        // drivers without an override (F469 today) are byte-identical:
        // the answer still carries the core's polled-cache presence
        let mut h = CcidMessageHandler::new(
            crate::mock_driver::MockSmartcardDriver::new().card_present(true),
            CHERRY,
        );
        let diag = escape_d0(&mut h);
        assert!(diag.card_present, "cache-fed presence still answers");
        assert_eq!(diag.apdu_tx_count, 0);
    }

    #[test]
    fn driver_reinit_event_refreshes_reinit_word_inline() {
        // #342: a driver self-healing re-init fires mid-session (the
        // mock latches it exactly as the real F469 wrapper does when
        // the failure streak reaches REINIT_THRESHOLD). The CCID layer
        // must consume the event on the command dispatch it rides on:
        // the 0xD0 reinit word refreshes immediately instead of
        // waiting for the next presence poll (check_card_change is
        // what normally copies driver reinit_count into diagnostics).
        let mut h = CcidMessageHandler::new(
            crate::mock_driver::MockSmartcardDriver::new()
                .card_present(true)
                .with_atr(&[0x3B, 0x00])
                .with_transmit_apdu_error(),
            CHERRY,
        );
        h.set_rx_data(&frame(PC_TO_RDR_ICC_POWER_ON, 1, &[]));
        h.handle_message();
        let _ = h.take_response();
        assert_eq!(h.diagnostics().reinit_count, 0);

        // The failing exchange during which the wrapper's threshold
        // logic fired the re-init — latched before dispatch returns,
        // as on real hardware.
        h.core.driver_mut().driver.set_reinit_event(1);
        h.set_rx_data(&frame(
            PC_TO_RDR_XFR_BLOCK,
            2,
            &[0x00, 0xC0, 0x00, 0x00, 0x00],
        ));
        h.handle_message();
        let (len, resp) = h.take_response();
        // The triggering exchange itself still answers honestly: a
        // FAILED DataBlock (the jnasmartcardio-safe shape), untouched
        // by the event surfacing.
        assert_eq!(resp[0], RDR_TO_PC_DATABLOCK);
        assert_eq!(len, 10, "empty DataBlock payload");
        assert_ne!(
            resp[7] & (COMMAND_STATUS_FAILED << 6),
            0,
            "the failing exchange reports FAILED"
        );

        let diag = escape_d0(&mut h);
        assert_eq!(
            diag.reinit_count, 1,
            "reinit word refreshed inline, not at the next presence poll"
        );
    }
}

#[cfg(test)]
mod xfr_error_map_tests {
    //! #136: the classification chain end-to-end on the firmware
    //! wrapper — mock class → SmartcardCoreAdapter forward →
    //! ccid-core error-map table → DataBlock bError byte. The table
    //! itself is byte-pinned in ccid-core; these lock the wiring.

    use super::*;
    use card_interface::XfrErrorKind;
    use ccid_protocol::types::CCID_ERR_XFR_PARITY_ERROR;

    const CHERRY: u16 = 0x046A;

    fn frame(msg_type: u8, seq: u8, payload: &[u8]) -> std::vec::Vec<u8> {
        let mut f = std::vec::Vec::with_capacity(10 + payload.len());
        f.push(msg_type);
        f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        f.push(0); // slot
        f.push(seq);
        f.extend_from_slice(&[0, 0, 0]); // bSpecific(3)
        f.extend_from_slice(payload);
        f
    }

    fn powered_on_handler(
        class: XfrErrorKind,
    ) -> CcidMessageHandler<crate::mock_driver::MockSmartcardDriver> {
        let mut h = CcidMessageHandler::new(
            crate::mock_driver::MockSmartcardDriver::new()
                .card_present(true)
                .with_atr(&[0x3B, 0x00])
                .with_transmit_apdu_error()
                .with_transmit_error_class(class),
            CHERRY,
        );
        h.set_rx_data(&frame(PC_TO_RDR_ICC_POWER_ON, 1, &[]));
        h.handle_message();
        let _ = h.take_response();
        h
    }

    fn xfr_b_error(h: &mut CcidMessageHandler<crate::mock_driver::MockSmartcardDriver>) -> u8 {
        h.set_rx_data(&frame(PC_TO_RDR_XFR_BLOCK, 2, &[0x00, 0xA4, 0x04, 0x00]));
        h.handle_message();
        let (len, resp) = h.take_response();
        assert_eq!(resp[0], RDR_TO_PC_DATABLOCK, "always a DataBlock");
        assert_eq!(len, 10, "honest dwLength=0 on failure");
        resp[8]
    }

    #[test]
    fn parity_class_surfaces_as_berror_0xfd() {
        let mut h = powered_on_handler(XfrErrorKind::Parity);
        assert_eq!(xfr_b_error(&mut h), 0xFD);
    }

    #[test]
    fn timeout_class_keeps_berror_0xf0() {
        let mut h = powered_on_handler(XfrErrorKind::Timeout);
        assert_eq!(xfr_b_error(&mut h), 0xF0);
    }

    #[test]
    fn unclassified_class_keeps_historical_fold() {
        let mut h = powered_on_handler(XfrErrorKind::Unclassified);
        assert_eq!(xfr_b_error(&mut h), 0xF0);
    }

    #[test]
    fn parity_maps_through_the_constant_not_a_literal() {
        assert_eq!(CCID_ERR_XFR_PARITY_ERROR, 0xFD);
    }
}

#[cfg(test)]
mod secure_counter_tests {
    //! #276 regression locks: PIN-pad Secure exchanges must advance the
    //! Escape 0xD0 apdu_tx/apdu_rx counters. Before the fix the wrapper
    //! called `driver_mut().transmit_apdu` directly — every PIN exchange
    //! was invisible to the 0xD0 diagnostics (the #241 class, alive on
    //! the Secure surface). These tests are deliberately NOT
    //! display-gated: the PIN-result exchange is pure handler logic (the
    //! display feature gates the ENTRY UI, not the card transmit).

    use super::*;

    const CHERRY: u16 = 0x046A;
    const ATR: [u8; 2] = [0x3B, 0x00];

    /// §6.1.11 probe in the pcscd wire shape (same bytes as the core's
    /// `pin_verify_probe`): wPINMaxExtraDigit wire bytes `[max, min]`,
    /// bTeoPrologue[3], APDU head at offset 14.
    fn pin_verify_payload() -> std::vec::Vec<u8> {
        let mut p = std::vec::Vec::new();
        p.push(0x00); // bPINOperation: verification
        p.extend_from_slice(&[
            30, 0x00, 0x00, 0x00, 8, 4, 0x02, 1, 0x09, 0x04, 0, 0, 0, 0, 0x00, 0x20, 0x00, 0x81,
        ]);
        p
    }

    /// §6.1.12 probe (same bytes as the core's `pin_modify_probe`).
    fn pin_modify_payload() -> std::vec::Vec<u8> {
        let mut d = [0u8; 23];
        d[0] = 30;
        d[4] = 5;
        d[5] = 13;
        d[6] = 8;
        d[7] = 4;
        d[8] = 3;
        d[9] = 0x02;
        d[10] = 3;
        d[13] = 0;
        d[14] = 1;
        d[15] = 2;
        d[19..23].copy_from_slice(&[0x00, 0x24, 0x00, 0x81]);
        let mut p = std::vec::Vec::new();
        p.push(0x01); // bPINOperation: modification
        p.extend_from_slice(&d);
        p
    }

    fn frame(msg_type: u8, seq: u8, payload: &[u8]) -> std::vec::Vec<u8> {
        let mut f = std::vec::Vec::with_capacity(10 + payload.len());
        f.push(msg_type);
        f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        f.push(0); // slot
        f.push(seq);
        f.extend_from_slice(&[0, 0, 0]); // bSpecific(3)
        f.extend_from_slice(payload);
        f
    }

    fn pin_buffer(digits: &[u8]) -> PinBuffer {
        let mut b = PinBuffer::new(8);
        for &d in digits {
            assert!(b.push(d));
        }
        b
    }

    fn armed_verify_handler() -> CcidMessageHandler<crate::mock_driver::MockSmartcardDriver> {
        let mut h = CcidMessageHandler::new(
            crate::mock_driver::MockSmartcardDriver::new()
                .card_present(true)
                .with_atr(&ATR)
                .with_apdu_response(&[0x90, 0x00]),
            CHERRY,
        );
        h.set_rx_data(&frame(PC_TO_RDR_ICC_POWER_ON, 1, &[]));
        h.handle_message();
        let _ = h.take_response();
        assert_eq!(h.slot_state(), SlotState::PresentActive);

        h.set_rx_data(&frame(PC_TO_RDR_SECURE, 0x55, &pin_verify_payload()));
        h.handle_message();
        let (len, _) = h.take_response();
        assert_eq!(len, 0, "Secure verify defers its response");
        h
    }

    #[test]
    fn pin_verify_exchange_advances_apdu_counters() {
        let mut h = armed_verify_handler();
        let (seq, params) = h.take_secure_params().expect("armed");
        assert_eq!(seq, 0x55);

        assert_eq!(h.diagnostics().apdu_tx_count, 0);
        assert_eq!(h.diagnostics().apdu_rx_count, 0);

        h.set_pin_result(
            seq,
            PinResult::Success,
            pin_buffer(&[1, 2, 3, 4, 5, 6]),
            params,
        );
        h.process_pin_result();

        let len = h.get_tx_len();
        assert_eq!(len, 10 + 2, "DataBlock carries the card SW");
        assert_eq!(h.get_tx_buffer()[0], RDR_TO_PC_DATABLOCK);
        assert_eq!(&h.get_tx_buffer()[len - 2..len], &[0x90, 0x00]);
        assert_eq!(
            h.diagnostics().apdu_tx_count,
            1,
            "the PIN VERIFY APDU must be counted"
        );
        assert_eq!(
            h.diagnostics().apdu_rx_count,
            1,
            "the card answer must be counted"
        );
    }

    #[test]
    fn pin_verify_failed_transmit_counts_tx_not_rx() {
        let mut h = CcidMessageHandler::new(
            crate::mock_driver::MockSmartcardDriver::new()
                .card_present(true)
                .with_atr(&ATR)
                .with_apdu_response(&[0x90, 0x00])
                .with_transmit_apdu_error(),
            CHERRY,
        );
        h.set_rx_data(&frame(PC_TO_RDR_ICC_POWER_ON, 1, &[]));
        h.handle_message();
        let _ = h.take_response();

        h.set_rx_data(&frame(PC_TO_RDR_SECURE, 0x55, &pin_verify_payload()));
        h.handle_message();
        let _ = h.take_response();
        let (seq, params) = h.take_secure_params().expect("armed");

        h.set_pin_result(
            seq,
            PinResult::Success,
            pin_buffer(&[1, 2, 3, 4, 5, 6]),
            params,
        );
        h.process_pin_result();

        // Cancelled answer, but the attempt still reached the card seam.
        assert!(h.get_tx_len() > 0);
        assert_eq!(h.diagnostics().apdu_tx_count, 1);
        assert_eq!(h.diagnostics().apdu_rx_count, 0);
    }

    #[test]
    fn pin_modify_exchange_advances_apdu_counters() {
        let mut h = CcidMessageHandler::new(
            crate::mock_driver::MockSmartcardDriver::new()
                .card_present(true)
                .with_atr(&ATR)
                .with_apdu_response(&[0x90, 0x00]),
            CHERRY,
        );
        h.set_rx_data(&frame(PC_TO_RDR_ICC_POWER_ON, 1, &[]));
        h.handle_message();
        let _ = h.take_response();

        h.set_rx_data(&frame(PC_TO_RDR_SECURE, 0x66, &pin_modify_payload()));
        h.handle_message();
        let _ = h.take_response();
        assert!(h.is_pin_modify_active());
        let (seq, params) = h.take_secure_modify_params().expect("armed");
        assert_eq!(seq, 0x66);

        h.set_pin_modify_result(
            seq,
            PinResult::Success,
            pin_buffer(&[9, 9, 9, 9, 9, 9]),
            pin_buffer(&[1, 2, 3, 4, 5, 6]),
            params,
        );
        h.process_pin_modify_result();

        let len = h.get_tx_len();
        assert_eq!(len, 10 + 2);
        assert_eq!(h.get_tx_buffer()[0], RDR_TO_PC_DATABLOCK);
        assert_eq!(h.diagnostics().apdu_tx_count, 1);
        assert_eq!(h.diagnostics().apdu_rx_count, 1);
    }
}
