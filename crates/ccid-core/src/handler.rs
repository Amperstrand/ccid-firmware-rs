//! Transport-agnostic CCID Rev 1.1 §6 message handler core.
//!
//! Extracted from the ESP32 `CcidHandler` (the younger, fuzz-hardened of
//! the two parallel handlers — docs/PERF_AND_DRY_STRATEGY.md §3.1) so
//! both firmwares can share one dispatch: slot lifecycle, bStatus
//! assembly, parameter handling, escape diagnostics, the APDU-level
//! pipeline (PPS echo, PC/SC pseudo-APDUs, card relay) and — since DRY
//! phase 2 (issue #121) — the contact-smartcard flavor (ATR-derived
//! parameters, host-parameter echo with libccid quirk validation,
//! contact error shapes) and the PC_to_RDR_Secure PIN verify/modify
//! surface live here, behind the card-interface driver traits.
//!
//! Reader families are keyed by the [`ReaderProfile`] type parameter:
//! [`SerialNfcProfile`] (the default) reproduces the pre-extraction
//! ESP32 GemPC-serial wire behavior byte-for-byte (conformance-battery
//! gate, issue #92 fuzz suite); [`ContactProfile`] reproduces the
//! STM32 contact-reader wire behavior pinned by the firmware host
//! suites. The PIN-pad capability is a runtime knob (`pin_pad`) because
//! it varies by device profile vendor id.
//!
//! What deliberately stays OUT (transport/firmware glue, kept by the
//! thin wrappers):
//! - T=1 endpoint routing (`esp32-ccid/src/t1.rs`, §3.4) — wired via the
//!   [`CcidHandlerCore::take_card_reset`] /
//!   [`CcidHandlerCore::take_parameters_accepted`] events and
//!   [`CcidHandlerCore::apdu_pipeline`]
//! - Escape vendor sets — the GemPC serial conventions (0x02 identity,
//!   0xD1 snapshot, 0x01-01-01 sync) live here for the serial profile,
//!   profile-gated off for contact (track I, #121); the shared 0xD0
//!   diagnostics answer lives here with the optional #344 driver fold
//!   ([`CcidHandlerCore::with_diagnostics_fold`]); the STM32 wrapper
//!   keeps its Gemalto 0x6A + vendor gating local (vendor flavor)
//! - IccClock / SetDataRateAndClockFrequency — smartcard-USART specifics,
//!   driven directly by the STM32 wrapper
//! - PIN-pad UI (STM32 `pinpad`) — the PIN-result→APDU exchange itself
//!   lives here (track I, issue #121:
//!   [`CcidHandlerCore::complete_pin_verify_exchange`] /
//!   [`CcidHandlerCore::complete_pin_modify_exchange`])
//! - Escape 0xD1 snapshot panic — the wrapper acks, the serve loop panics

use crate::diagnostics::Diagnostics;
use crate::params::default_params;
use crate::pps::is_pps_request;
use crate::response::{write_data_block, write_message, write_slot_status};
use crate::secure::{
    ModifyApduBuilder, PinBuffer, PinModifyParams, PinResult, PinVerifyParams, SecureState,
    VerifyApduBuilder,
};
use card_interface::{PresenceState, SlotDriver, XfrErrorKind};
use ccid_protocol::atr::{
    di_from_ta1_low, fi_from_ta1_high, parse_atr, t1_bwi_cwi_ifsc, AtrParams,
};
use ccid_protocol::status::build_bstatus;
use ccid_protocol::types::{
    CcidHeader, SlotState, CCID_ERR_CARD_TIMEOUT, CCID_ERR_CMD_ABORTED, CCID_ERR_CMD_NOT_SUPPORTED,
    CCID_ERR_CMD_SLOT_BUSY, CCID_ERR_HW_ERROR, CCID_ERR_ICC_MUTE, CCID_ERR_PIN_CANCELLED,
    CCID_ERR_PIN_TIMEOUT, CCID_ERR_XFR_OVERRUN, CCID_ERR_XFR_PARITY_ERROR, CCID_HEADER_SIZE,
    COMMAND_STATUS_FAILED, COMMAND_STATUS_NO_ERROR, ICC_STATUS_NO_ICC, ICC_STATUS_PRESENT_ACTIVE,
    ICC_STATUS_PRESENT_INACTIVE, PC_TO_RDR_ABORT, PC_TO_RDR_ESCAPE, PC_TO_RDR_GET_PARAMETERS,
    PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_POWER_OFF, PC_TO_RDR_ICC_POWER_ON,
    PC_TO_RDR_RESET_PARAMETERS, PC_TO_RDR_SECURE, PC_TO_RDR_SET_PARAMETERS, PC_TO_RDR_XFR_BLOCK,
    RDR_TO_PC_DATABLOCK, RDR_TO_PC_ESCAPE, RDR_TO_PC_PARAMETERS,
};

/// GemPC Twin serial convention (not a CCID Rev 1.1 Table 6.2-2 code):
/// XfrBlock against a present-but-inactive slot.
pub const ICC_NOT_ACTIVE: u8 = 0x05;

// The XfrBlock failure error-map table (issue #136): driver error
// class → CCID bError, every entry pinned to what the HOST driver
// actually does with that byte. The authority is libccid's
// `CCID_Receive` FAILED-status switch — the exact code path every
// pcscd/libccidtwin session runs on our DataBlock answers:
//
// LIBCCID: switch (cmd[ERROR_OFFSET]) ...
// LIBCCID: case 0xEF:	/* cancel */ ...
// LIBCCID: rx_buffer[0]= 0x64;
// LIBCCID: rx_buffer[1]= 0x01; ...
// LIBCCID: case 0xF0:	/* timeout */ ...
// LIBCCID: rx_buffer[0]= 0x64;
// LIBCCID: rx_buffer[1]= 0x00; ...
// LIBCCID: case 0xFD:	/* Parity error during exchange */ ...
// LIBCCID: return IFD_PARITY_ERROR; ...
// LIBCCID: case 0xFE:	/* Card absent or mute */ ...
// LIBCCID: if (2 == (cmd[STATUS_OFFSET] & 0x02)) /* No ICC */ ...
// LIBCCID: return IFD_ICC_NOT_PRESENT; ...
// LIBCCID: else ...
// LIBCCID: return IFD_COMMUNICATION_ERROR; ...
// LIBCCID: default: ...
// LIBCCID: return IFD_COMMUNICATION_ERROR;

// (`reference/CCID/src/commands.c` `CCID_Receive`, upstream CCID
// 1.7.1-8 @ 1a0f75d.) Host consequences per entry:
// | class        | bError | libccid result              | pcscd/host effect |
// |--------------|--------|-----------------------------|-------------------|
// | Parity       | 0xFD   | IFD_PARITY_ERROR            | T=0/T=1 engine error path — the HOST owns the retry ladder (reader-role doctrine) |
// | Overrun      | 0xFC   | (default) comm error        | SCardTransmit fails; host re-syncs |
// | Timeout      | 0xF0   | synthesizes SW `64 00`      | clean card error the whole host stack reports (jnasmartcardio-safe) |
// | Mute         | 0xFE   | ICC-not-present / comm err  | card-mute error (osmo WTIME_EXP shape) |
// | Hardware     | 0xFB   | (default) comm error        | SCardTransmit fails |
// | Unclassified | 0xF0   | synthesizes SW `64 00`      | historical fold-everything shape (behavior-preserving default) |
pub fn xfr_failure_b_error(kind: XfrErrorKind) -> u8 {
    match kind {
        // Wire corruption the reader detected but cannot repair:
        // answer with the code libccid turns into IFD_PARITY_ERROR so
        // the host protocol engine (owner of T=0/T=1 retries) takes
        // over — never fold this into 0xF0, which would synthesize a
        // SW 64 00 "card answered negatively" the card never sent.
        // LIBCCID: case 0xFD:	/* Parity error during exchange */
        // LIBCCID: 	return IFD_PARITY_ERROR;
        XfrErrorKind::Parity => CCID_ERR_XFR_PARITY_ERROR,
        // Lost bytes (ORE-class): the stream is shredded, honest error.
        // Falls into the libccid default arm → IFD_COMMUNICATION_ERROR
        // (SCardTransmit fails, host re-syncs) — no SW synthesis.
        // LIBCCID: default:
        // LIBCCID: 	return IFD_COMMUNICATION_ERROR;
        XfrErrorKind::Overrun => CCID_ERR_XFR_OVERRUN,
        // Card timed out inside the waiting-time budget: the shape the
        // #132 fix bench-proved through pcscd — libccid synthesizes
        // SW 64 00 and returns IFD_SUCCESS, so the whole host stack
        // (pcscd, pyscard, GlobalPlatformPro) reports a clean card
        // error instead of a driver crash (jnasmartcardio position(-2),
        // gp #307).
        // LIBCCID: case 0xF0:	/* timeout */
        // LIBCCID: 	rx_buffer[0]= 0x64;
        // LIBCCID: 	rx_buffer[1]= 0x00;
        // LIBCCID: ...
        // LIBCCID: 	return IFD_SUCCESS;
        XfrErrorKind::Timeout => CCID_ERR_CARD_TIMEOUT,
        // Card mute: what osmo-ccid-firmware answers on waiting-time
        // expiry (deactivate + FAILED DataBlock with ICC_MUTE) — the
        // free-software reference for the same hardware situation.
        // With the ICC-present status bit libccid maps this to
        // IFD_COMMUNICATION_ERROR.
        // OSMO_FSM: resp = ccid_gen_data_block(cs, ss->seq, CCID_CMD_STATUS_FAILED, CCID_ERR_ICC_MUTE, 0, 0);
        XfrErrorKind::Mute => CCID_ERR_ICC_MUTE,
        // Reader-side electrical/driver failure: honest hardware error.
        // LIBCCID: default:
        // LIBCCID: 	return IFD_COMMUNICATION_ERROR;
        XfrErrorKind::Hardware => CCID_ERR_HW_ERROR,
        // Backends that cannot classify keep the bench-proven fold:
        // 0xF0 → SW 64 00 (same host behavior as Timeout; chosen
        // deliberately over a hardware-error fold so unmodified
        // drivers cannot regress pcscd sessions — see the #132
        // honest-error contract discussion).
        XfrErrorKind::Unclassified => CCID_ERR_CARD_TIMEOUT,
    }
}

/// Reader-family flavor knobs where the contact (USB, Short-APDU) and
/// contactless serial (GemPC Twin) handlers historically differ in wire
/// bytes (PERF_AND_DRY_STRATEGY §3.1 phase 2).
pub trait ReaderProfile {
    /// Contact smartcard flavor: IccPowerOn voltage/dwLength validation
    /// with ICC_MUTE error codes, ATR-derived protocol + parameters,
    /// SetParameters field validation with host-parameter echo, contact
    /// XfrBlock error shapes (0xFE/0xFF), T=0 default parameters,
    /// Abort-slot acknowledgment, power-off driver on card removal.
    const CONTACT: bool;

    /// Reader-level PC/SC pseudo-APDUs (CLA=0xFF, e.g. GET UID) are
    /// answered by the reader itself. Contactless serial convention;
    /// contact readers forward everything but PPS to the card.
    const PSEUDO_APDUS: bool;
}

/// ESP32 GemPC-serial profile (the default): byte-identical to the
/// pre-extraction ESP32 handler.
pub struct SerialNfcProfile;

impl ReaderProfile for SerialNfcProfile {
    const CONTACT: bool = false;
    const PSEUDO_APDUS: bool = true;
}

/// STM32 contact-smartcard profile: byte-identical to the pre-port
/// STM32 `CcidMessageHandler`.
pub struct ContactProfile;
impl ReaderProfile for ContactProfile {
    const CONTACT: bool = true;
    const PSEUDO_APDUS: bool = false;
}

/// Wire timing of the last accepted SetParameters on a contact reader
/// (#122 AUTO_BAUD plumbing): what a dwFeatures bit-5 reader must apply
/// to its USART after the host negotiates. Produced by
/// [`CcidHandlerCore::accepted_contact_timing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContactWireTiming {
    /// Fi (clock conversion factor, e.g. 372 at TA1=0x11/0x18).
    pub fi: u16,
    /// Di (baud rate adjustment, e.g. 8 at TA1=0x18).
    pub di: u8,
    /// bGuardTime (T0: bGuardTimeT0 / T1: bGuardTimeT1) — raw N.
    pub guard_time_n: u8,
    /// 0 = T=0, 1 = T=1.
    pub protocol: u8,
}

pub struct CcidHandlerCore<D: SlotDriver, P: ReaderProfile = SerialNfcProfile> {
    nfc: D,
    slot_state: SlotState,
    presence_state: PresenceState,
    tx_buf: [u8; 271],
    sync_notifications: bool,
    diagnostic_snapshot_requested: bool,
    current_protocol: u8,
    diagnostics: Diagnostics,
    firmware_version: &'static [u8],
    card_reset_event: bool,
    parameters_accepted_event: bool,
    /// PC_to_RDR_Secure capability (bPINSupport != 0). Runtime knob: it
    /// varies by device-profile vendor id, which the wrapper knows.
    pin_pad: bool,
    secure_state: SecureState,
    /// Host-supplied parameter set from PC_to_RDR_SetParameters:
    /// (bProtocolNum, raw abProtocolDataStructure). Takes precedence over
    /// the ATR-derived values in GetParameters/SetParameters responses
    /// until the next power-on or ResetParameters. Contact profile only.
    host_params: Option<(u8, [u8; 7])>,
    /// ATR-derived parameter source (contact profile); refreshed on each
    /// successful IccPowerOn.
    atr_params: AtrParams,
    /// #344 driver-known-state overlay for the Escape 0xD0 answer:
    /// drivers whose live view is fresher than the polled cache (the
    /// F746 bit-bang wrapper's presence pin) fold it in AT ANSWER TIME.
    /// `None` = the cached snapshot answers as-is (default; the ESP32
    /// wrapper and the pre-fold F469 shape).
    diagnostics_fold: Option<fn(&D, &mut Diagnostics)>,
    _profile: core::marker::PhantomData<P>,
}

impl<D: SlotDriver> CcidHandlerCore<D, SerialNfcProfile> {
    /// `firmware_version` is the string returned by Escape 0x02 (reader
    /// identity on the wire — GemPC Twin convention).
    pub fn new(nfc: D, firmware_version: &'static [u8]) -> Self {
        Self::with_profile(nfc, firmware_version, false)
    }
}

impl<D: SlotDriver, P: ReaderProfile> CcidHandlerCore<D, P> {
    /// Profile-aware constructor. `pin_pad` enables the PC_to_RDR_Secure
    /// surface (pin-pad readers only — e.g. the Cherry ST-2xxx vendor id
    /// on the STM32); non-pin-pad readers answer Secure with
    /// CMD_NOT_SUPPORTED in their profile's response shape.
    pub fn with_profile(nfc: D, firmware_version: &'static [u8], pin_pad: bool) -> Self {
        let mut handler = Self {
            nfc,
            slot_state: SlotState::Absent,
            presence_state: PresenceState { present: false },
            tx_buf: [0u8; 271],
            sync_notifications: false,
            diagnostic_snapshot_requested: false,
            // T=1 (contactless ISO-DEP) reader default; the contact
            // profile's wrapper re-seeds this from the first ATR.
            current_protocol: if P::CONTACT { 0 } else { 1 },
            diagnostics: Diagnostics::new(),
            firmware_version,
            card_reset_event: false,
            parameters_accepted_event: false,
            pin_pad,
            secure_state: SecureState::Idle,
            host_params: None,
            atr_params: AtrParams::default(),
            diagnostics_fold: None,
            _profile: core::marker::PhantomData,
        };
        // Sync recovery history at construction (Codex reviews on #46/#42):
        // startup retries may already have reinitialized the driver before
        // the handler existed; without this the first 0xD0 query reports 0.
        handler.diagnostics.reinit_count = handler.nfc.reinit_count();
        handler
    }

    /// #344 driver-diagnostics fold: overlay driver-known state onto the
    /// Escape 0xD0 answer at ANSWER TIME (the polled cache only refreshes
    /// on a presence poll). The fold must ADD its state — resetting
    /// core-owned counters (apdu_tx/rx, uptime) is the #268 failure mode;
    /// the firmware's fold tests pin the contract. Inert unless installed.
    pub fn with_diagnostics_fold(mut self, fold: fn(&D, &mut Diagnostics)) -> Self {
        self.diagnostics_fold = Some(fold);
        self
    }

    /// Escape 0xD1 arms a diagnostic snapshot: the response is acked, the
    /// serve loop consumes this flag AFTER the ack went out and panics
    /// with a marker reason so the panic handler writes a flash coredump
    /// for post-mortem retrieval (AGENTS.md "Crash dumps & snapshots").
    pub fn take_snapshot_request(&mut self) -> bool {
        core::mem::replace(&mut self.diagnostic_snapshot_requested, false)
    }

    /// Consumed by transport glue after every delegated command: set on
    /// each successful IccPowerOn. A card reset invalidates the
    /// negotiated protocol (CCID §6.1.2: parameters do not survive ATR
    /// reset) — protocol-dependent routes (e.g. the ESP32 T=1 endpoint)
    /// must disarm; SetParameters re-arms via
    /// [`Self::take_parameters_accepted`].
    pub fn take_card_reset(&mut self) -> bool {
        core::mem::replace(&mut self.card_reset_event, false)
    }

    /// Consumed by transport glue after every delegated command: set when
    /// SetParameters/ResetParameters was ACCEPTED (invalid bProtocolNum
    /// rejections never set it).
    pub fn take_parameters_accepted(&mut self) -> bool {
        core::mem::replace(&mut self.parameters_accepted_event, false)
    }

    /// The negotiated T=1 timing (BWI, CWI, IFSC) from the last accepted
    /// SetParameters — for protocol endpoints that must honor the
    /// windows the host believes (G-04).
    pub fn applied_t1_timing(&self) -> Option<(u8, u8, u8)> {
        let (proto, raw) = self.host_params?;
        if proto != 1 || self.current_protocol != 1 {
            return None;
        }
        Some(t1_bwi_cwi_ifsc(&raw))
    }

    /// The wire timing of the last ACCEPTED SetParameters — the values a
    /// dwFeatures bit-5 ("Automatic baud rate change according to
    /// frequency and Fi, Di params") reader must apply to its USART:
    /// Fi/Di from abProtocolDataStructure byte 0, guard time from byte 2
    /// (bGuardTimeT0 for T=0 / bGuardTimeT1 for T=1), plus the protocol.
    ///
    /// Host contract (#122): libccid's IFDHSetProtocolParameters fills
    /// these from the ATR TA1 (and its own PPS when it owns PPS) and
    /// sends them AFTER IccPowerOn — the reader is expected to bring its
    /// wire rate to match. `None` until a SetParameters/ResetParameters
    /// was accepted (and ResetParameters clears the override — the ATR
    /// defaults govern again).
    pub fn accepted_contact_timing(&self) -> Option<ContactWireTiming> {
        let (proto, raw) = self.host_params?;
        Some(ContactWireTiming {
            fi: fi_from_ta1_high(raw[0] >> 4),
            di: di_from_ta1_low(raw[0] & 0x0F),
            guard_time_n: raw[2],
            protocol: proto,
        })
    }

    /// True while a PC_to_RDR_Secure PIN session is armed (verify or
    /// modify) and awaiting the wrapper's `complete_pin_entry`.
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

    /// Takes the armed PIN-verify request (bSeq + §6.1.11 parameters) for
    /// the wrapper's pin-pad UI — CONSUMES the armed state (the wrapper's
    /// poll loop branches on [`Self::is_pin_entry_active`] once per
    /// session; consuming prevents restart loops).
    pub fn take_secure_params(&mut self) -> Option<(u8, PinVerifyParams)> {
        if let SecureState::WaitingForPinVerify { seq, params } =
            core::mem::replace(&mut self.secure_state, SecureState::Idle)
        {
            Some((seq, params))
        } else {
            None
        }
    }

    /// Takes the armed PIN-modify request (bSeq + §6.1.12 parameters).
    /// Same consuming contract as [`Self::take_secure_params`].
    pub fn take_secure_modify_params(&mut self) -> Option<(u8, PinModifyParams)> {
        if let SecureState::WaitingForPinModify { seq, params } =
            core::mem::replace(&mut self.secure_state, SecureState::Idle)
        {
            Some((seq, params))
        } else {
            None
        }
    }

    /// Drops any armed PIN session (card removal, wrapper teardown).
    pub fn cancel_secure(&mut self) {
        self.secure_state = SecureState::Idle;
    }

    /// Completes a PIN session with the UI outcome and (on success) the
    /// card's answer to the reader-built VERIFY/CHANGE APDU: writes the
    /// RDR_to_PC_DataBlock response into `response` and returns its
    /// length. Response shapes are byte-identical to the pre-port STM32
    /// handler (issue #121 gate).
    pub fn complete_pin_entry(
        &mut self,
        seq: u8,
        result: PinResult,
        apdu_response: Option<&[u8]>,
        response: &mut [u8],
    ) -> usize {
        let icc_status = self.current_icc_status();

        let (cmd_status, error, data): (u8, u8, &[u8]) = match result {
            PinResult::Success => (COMMAND_STATUS_NO_ERROR, 0, apdu_response.unwrap_or(&[])),
            PinResult::Cancelled => (COMMAND_STATUS_FAILED, CCID_ERR_PIN_CANCELLED, &[]),
            PinResult::Timeout => (COMMAND_STATUS_FAILED, CCID_ERR_PIN_TIMEOUT, &[]),
            PinResult::InvalidLength => (COMMAND_STATUS_FAILED, CCID_ERR_CMD_ABORTED, &[]),
        };

        let len = write_message(
            RDR_TO_PC_DATABLOCK,
            0,
            seq,
            build_bstatus(cmd_status, icc_status),
            error,
            0,
            data,
            response,
        );
        self.secure_state = SecureState::Idle;
        len
    }

    /// PIN-verify result exchange — the tail of the §6.1.11 surface,
    /// moved from the STM32 wrapper (issue #121 track I): build the
    /// VERIFY APDU from the host-supplied §6.1.11 template + the
    /// entered buffer, transmit through the counted seam (#276: PIN
    /// exchanges must advance the Escape 0xD0 apdu counters exactly
    /// like XfrBlock traffic) and write the RDR_to_PC_DataBlock via
    /// [`Self::complete_pin_entry`]. Failure mapping is the wrapper's
    /// pre-port shape, pinned by the firmware suite: a card-transmit
    /// failure answers Cancelled (0xEF), an APDU-build rejection
    /// answers InvalidLength (0xEB).
    pub fn complete_pin_verify_exchange(
        &mut self,
        seq: u8,
        result: PinResult,
        buffer: PinBuffer,
        params: &PinVerifyParams,
        response: &mut [u8],
    ) -> usize {
        match result {
            PinResult::Success => {
                let ascii_pin = buffer.to_ascii();
                let pin_len = buffer.len();
                let builder = VerifyApduBuilder::from_template(
                    params.apdu_template[0],
                    params.apdu_template[2],
                    params.apdu_template[3],
                );
                match builder.build(&ascii_pin[..pin_len]) {
                    Ok(apdu) => {
                        // Card-answer scratch: same 261-byte budget the
                        // wrapper kept as a field; a transient stack
                        // frame is no worse than the pre-port resp_copy.
                        let mut card_resp = [0u8; 261];
                        match self.transmit_counted(&apdu[..5 + pin_len], &mut card_resp) {
                            Ok(resp_len) => self.complete_pin_entry(
                                seq,
                                PinResult::Success,
                                Some(&card_resp[..resp_len]),
                                response,
                            ),
                            Err(_) => {
                                self.complete_pin_entry(seq, PinResult::Cancelled, None, response)
                            }
                        }
                    }
                    Err(_) => {
                        self.complete_pin_entry(seq, PinResult::InvalidLength, None, response)
                    }
                }
            }
            PinResult::Cancelled | PinResult::Timeout | PinResult::InvalidLength => {
                self.complete_pin_entry(seq, result, None, response)
            }
        }
    }

    /// PIN-modify result exchange — the §6.1.12 twin of
    /// [`Self::complete_pin_verify_exchange`] (issue #121 track I):
    /// build the CHANGE REFERENCE DATA APDU (old + new PIN at the
    /// host-supplied insertion offsets), transmit counted, complete.
    pub fn complete_pin_modify_exchange(
        &mut self,
        seq: u8,
        result: PinResult,
        old_buffer: PinBuffer,
        new_buffer: PinBuffer,
        params: &PinModifyParams,
        response: &mut [u8],
    ) -> usize {
        match result {
            PinResult::Success => {
                let old_pin = old_buffer.to_ascii();
                let old_len = old_buffer.len();
                let new_pin = new_buffer.to_ascii();
                let new_len = new_buffer.len();
                let builder = ModifyApduBuilder::from_template(
                    params.apdu_template[0],
                    params.apdu_template[2],
                    params.apdu_template[3],
                    params.old_pin_offset as usize,
                    params.new_pin_offset as usize,
                );
                match builder.build(&old_pin[..old_len], &new_pin[..new_len]) {
                    Ok(apdu) => {
                        let mut card_resp = [0u8; 261];
                        let apdu_len = 5 + old_len + new_len;
                        match self.transmit_counted(&apdu[..apdu_len], &mut card_resp) {
                            Ok(resp_len) => self.complete_pin_entry(
                                seq,
                                PinResult::Success,
                                Some(&card_resp[..resp_len]),
                                response,
                            ),
                            Err(_) => {
                                self.complete_pin_entry(seq, PinResult::Cancelled, None, response)
                            }
                        }
                    }
                    Err(_) => {
                        self.complete_pin_entry(seq, PinResult::InvalidLength, None, response)
                    }
                }
            }
            PinResult::Cancelled | PinResult::Timeout | PinResult::InvalidLength => {
                self.complete_pin_entry(seq, result, None, response)
            }
        }
    }

    /// Refresh time-varying diagnostic fields. Called from the firmware's
    /// poll loop; the handler stays host-testable by taking the tick count
    /// as a parameter instead of reading a platform timer itself.
    pub fn refresh_diagnostics(&mut self, uptime_ticks: u32) {
        self.diagnostics.uptime_ticks = uptime_ticks;
        self.diagnostics.reinit_count = self.nfc.reinit_count();
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    pub fn driver_mut(&mut self) -> &mut D {
        &mut self.nfc
    }

    pub fn driver(&self) -> &D {
        &self.nfc
    }

    pub fn diagnostics_mut(&mut self) -> &mut Diagnostics {
        &mut self.diagnostics
    }

    pub fn record_nak(&mut self) {
        self.diagnostics.nak_count = self.diagnostics.nak_count.saturating_add(1);
    }

    /// Counts one protocol error (truncation guard, failed responses) —
    /// the same accounting `process_command` applies internally, exposed
    /// for wrapper-side dispatch paths (T=1 route).
    pub fn record_protocol_error(&mut self) {
        self.diagnostics.error_count = self.diagnostics.error_count.saturating_add(1);
    }

    /// Transmit an APDU through the core's counting seam (issue #276):
    /// apdu_tx is counted on every call, apdu_rx on every card answer.
    /// The ONLY legal card-transmit path for wrapper-driven exchanges
    /// (the STM32 PIN-pad Verify/Modify APDUs) — the Escape 0xD0
    /// diagnostics were blind to PIN exchanges while the wrapper called
    /// `driver_mut().transmit_apdu` directly. The XfrBlock paths count
    /// through the same helper below.
    pub fn transmit_counted(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, D::Error> {
        Self::count_transmit(&mut self.diagnostics, &mut self.nfc, command, response)
    }

    /// Count-then-transmit — the ONE definition of when apdu_tx/apdu_rx
    /// advance: at the moment of actual card transfer (the #46/#43/#38
    /// convention — rejected/inactive requests and locally answered
    /// frames are not APDUs sent to the card). A failed transmit counts
    /// tx but not rx.
    fn count_transmit<D2: SlotDriver>(
        diagnostics: &mut Diagnostics,
        nfc: &mut D2,
        command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, D2::Error> {
        diagnostics.apdu_tx_count = diagnostics.apdu_tx_count.saturating_add(1);
        let result = nfc.transmit_apdu(command, response);
        if result.is_ok() {
            diagnostics.apdu_rx_count = diagnostics.apdu_rx_count.saturating_add(1);
        }
        result
    }

    pub fn slot_state(&self) -> SlotState {
        self.slot_state
    }

    pub fn current_protocol(&self) -> u8 {
        self.current_protocol
    }

    /// Mirrors the protocol-state effect of an accepted SetParameters
    /// (without emitting a response) — host tooling / tests.
    pub fn set_current_protocol(&mut self, protocol: u8) {
        self.current_protocol = protocol;
    }

    pub fn presence_state(&self) -> PresenceState {
        self.presence_state
    }

    pub fn sync_notifications(&self) -> bool {
        self.sync_notifications
    }

    pub fn process_command(&mut self, ccid_msg: &[u8], response: &mut [u8]) -> usize {
        let Some(header) = CcidHeader::parse(ccid_msg) else {
            return 0;
        };

        let payload_len = header.length as usize;
        if ccid_msg.len() < CCID_HEADER_SIZE + payload_len {
            // Input shorter than its declared dwLength. WIRE frames can
            // never arrive here truncated: the serial server's frame
            // parser only emits complete frames and its inter-byte
            // stall guard abandons partial ones before dispatch (issue
            // #114, Codex on #93). This path guards DIRECT
            // process_command input (host tests, the handler fuzz
            // suite) and counts a protocol error (Codex reviews on
            // #40/#38); the early return bypasses the post-dispatch
            // status accounting below.
            self.record_protocol_error();
            return write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_FAILED,
                CCID_ERR_CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }

        let payload = &ccid_msg[CCID_HEADER_SIZE..CCID_HEADER_SIZE + payload_len];

        let len = match header.message_type {
            PC_TO_RDR_ICC_POWER_ON => self.handle_power_on(&header, response),
            PC_TO_RDR_ICC_POWER_OFF => self.handle_power_off(&header, response),
            PC_TO_RDR_GET_SLOT_STATUS => self.handle_get_slot_status(&header, response),
            PC_TO_RDR_XFR_BLOCK => self.handle_xfr_block(&header, payload, response),
            PC_TO_RDR_GET_PARAMETERS => self.write_parameters(&header, response),
            PC_TO_RDR_SET_PARAMETERS => self.handle_set_parameters(&header, payload, response),
            PC_TO_RDR_RESET_PARAMETERS => self.handle_reset_parameters(&header, response),
            PC_TO_RDR_ESCAPE => self.handle_escape(&header, payload, response),
            PC_TO_RDR_SECURE => self.handle_secure(&header, payload, response),
            // Contact profile: single-slot synchronous reader — Abort is a
            // documented stub that always succeeds (CCID §6.1.13; nothing
            // is ever in flight when the next bulk-OUT message is read).
            // The serial profile keeps the pre-port behavior: unknown
            // command → CMD_NOT_SUPPORTED SlotStatus.
            PC_TO_RDR_ABORT if P::CONTACT => write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_NO_ERROR,
                0,
                0,
                response,
            ),
            _ => write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_FAILED,
                CCID_ERR_CMD_NOT_SUPPORTED,
                0,
                response,
            ),
        };

        // bmCommandStatus FAILED lives in bStatus bits [7:6]: osmo
        // ccid_proto.h has CCID_CMD_STATUS_FAILED 0x40 (pre-shifted) and
        // CCID_CMD_STATUS_MASK 0xC0; our constants are the raw 2-bit
        // values, so the bit is (COMMAND_STATUS_FAILED << 6).
        if len > 7 && response[7] & (COMMAND_STATUS_FAILED << 6) != 0 {
            self.diagnostics.error_count = self.diagnostics.error_count.saturating_add(1);
        }

        len
    }

    pub fn check_card_change(&mut self) -> Option<bool> {
        if self.nfc.session_active() {
            self.presence_state = PresenceState { present: true };
            self.slot_state = SlotState::PresentActive;
            self.diagnostics.card_present = true;
            return None;
        }

        let presence = self.nfc.poll_card_presence();
        self.diagnostics.card_present = presence.present;
        self.diagnostics.reinit_count = self.nfc.reinit_count();

        // #444 (bench 2026-10-23, deaf-after-~7-cycles soft wedge): a
        // driver can tear down an active session MID-EXCHANGE (the RF
        // error path clears the session and parks the lifecycle) while
        // the card STAYS coupled — presence never flaps, so the slot
        // would sit stale-PresentActive with a dead session and the
        // host would error-loop against it until ITS OWN restart (the
        // observed pcscd-reinit-only recovery). Detect the
        // active-to-inactive-without-removal transition and treat it as
        // a card reset: PresentInactive + the card-reset event
        // (transport glue disarms protocol routes off it — the serial
        // T=1 endpoint holds ns/chain state for the dead session's
        // conversation) + a change event so the host reconnects through
        // the standard power-on path instead of hammering transacts.
        if self.slot_state == SlotState::PresentActive && presence.present {
            self.slot_state = SlotState::PresentInactive;
            self.presence_state = presence;
            self.card_reset_event = true;
            return Some(true);
        }

        if presence.present != self.presence_state.present {
            self.presence_state = presence;
            if presence.present {
                self.slot_state = SlotState::PresentInactive;
            } else {
                // Contact profile quirk (pinned by the firmware suite):
                // removal powers the driver off and drops any armed PIN
                // session. The serial/NFC path leaves both to the driver
                // and wrapper (byte-identical to the pre-port handlers).
                if P::CONTACT {
                    self.nfc.power_off();
                    self.secure_state = SecureState::Idle;
                }
                self.slot_state = SlotState::Absent;
            }
            Some(presence.present)
        } else {
            None
        }
    }

    // CCID_SPEC: /* Section 6.1.1 */ struct ccid_pc_to_rdr_icc_power_on {
    // struct ccid_header hdr; uint8_t bPowerSelect; uint8_t abRFU[2]; }
    // __attribute__ ((packed)); /* Response: RDR_to_PC_DataBlock */
    fn handle_power_on(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        if P::CONTACT {
            return self.handle_power_on_contact(header, response);
        }
        if !self.presence_state.present {
            return write_slot_status(
                header.slot,
                header.seq,
                ICC_STATUS_NO_ICC,
                COMMAND_STATUS_FAILED,
                CCID_ERR_CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }

        match self.nfc.power_on(&mut self.tx_buf) {
            Ok(atr_len) => {
                self.presence_state.present = true;
                self.slot_state = SlotState::PresentActive;
                self.card_reset_event = true;
                write_message(
                    RDR_TO_PC_DATABLOCK,
                    header.slot,
                    header.seq,
                    build_bstatus(
                        COMMAND_STATUS_NO_ERROR,
                        SlotState::PresentActive.icc_status(),
                    ),
                    0,
                    0,
                    &self.tx_buf[..atr_len],
                    response,
                )
            }
            Err(e) => {
                // Bring-up diagnostics (bench 2026-10-08): the activation
                // failure reason is otherwise invisible.
                log::warn!("power_on failed: {:?}", e);
                // Don't poll after activation failure — the card may be in an
                // uncertain ISO 14443-3A state (e.g. READY after a partial
                // WUPA). Polling now could send WUPA from READY which keeps
                // the card stuck, breaking the next PowerUp attempt.
                // Instead, assume the card is still physically present and let
                // the next scheduled poll cycle verify.
                self.slot_state = SlotState::PresentInactive;
                write_slot_status(
                    header.slot,
                    header.seq,
                    self.current_icc_status(),
                    COMMAND_STATUS_FAILED,
                    CCID_ERR_HW_ERROR,
                    0,
                    response,
                )
            }
        }
    }

    // CCID_SPEC: /* Section 6.1.1 */ enum ccid_power_select { CCID_PWRSEL_AUTO = 0x00,
    // CCID_PWRSEL_5V0 = 0x01, CCID_PWRSEL_3V0 = 0x02, CCID_PWRSEL_1V8 = 0x03, };

    // Contact-profile IccPowerOn, ported byte-for-byte from the STM32
    // CcidMessageHandler (issue #121): dwLength validation, live card
    // presence, 5V/auto-only power select, ATR-derived protocol.
    fn handle_power_on_contact(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        if header.length != 0 {
            return write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_FAILED,
                CCID_ERR_CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }

        if !self.nfc.is_card_present() {
            return write_slot_status(
                header.slot,
                header.seq,
                ICC_STATUS_NO_ICC,
                COMMAND_STATUS_FAILED,
                CCID_ERR_ICC_MUTE,
                0,
                response,
            );
        }

        let power_select = header.specific[0];
        if power_select == 0x02 || power_select == 0x03 {
            return write_slot_status(
                header.slot,
                header.seq,
                ICC_STATUS_PRESENT_INACTIVE,
                COMMAND_STATUS_FAILED,
                CCID_ERR_CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }

        match self.nfc.power_on(&mut self.tx_buf) {
            Ok(atr_len) => {
                self.presence_state.present = true;
                self.slot_state = SlotState::PresentActive;
                self.atr_params = parse_atr(&self.tx_buf[..atr_len]);
                self.current_protocol = self.atr_params.protocol;
                self.host_params = None;
                self.card_reset_event = true;
                write_message(
                    RDR_TO_PC_DATABLOCK,
                    header.slot,
                    header.seq,
                    build_bstatus(
                        COMMAND_STATUS_NO_ERROR,
                        SlotState::PresentActive.icc_status(),
                    ),
                    0,
                    0,
                    &self.tx_buf[..atr_len],
                    response,
                )
            }
            Err(e) => {
                log::warn!("power_on failed: {:?}", e);
                self.slot_state = SlotState::PresentInactive;
                write_slot_status(
                    header.slot,
                    header.seq,
                    ICC_STATUS_PRESENT_INACTIVE,
                    COMMAND_STATUS_FAILED,
                    CCID_ERR_ICC_MUTE,
                    0,
                    response,
                )
            }
        }
    }

    // CCID_SPEC: /* Section 6.1.2 */ struct ccid_pc_to_rdr_icc_power_off {
    // struct ccid_header hdr; uint8_t abRFU[3]; } __attribute__ ((packed));
    // /* Response: RDR_to_PC_SlotStatus */
    fn handle_power_off(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        self.nfc.power_off();

        // After DESELECT the card is in HALT state. Do NOT poll here —
        // WUPA would move it to READY, and the next PowerUp's WUPA would
        // fail (WUPA is only valid from IDLE/HALT, not READY).
        // pcscd does PowerUp→PowerDown→PowerUp as a warm reset sequence,
        // so the next PowerUp must succeed.
        self.slot_state = SlotState::PresentInactive;
        if P::CONTACT {
            // Contact readers drop back to the T=0 default protocol
            // (pinned by the firmware suite: GetParameters after PowerOff
            // reports the 5-byte T=0 structure).
            self.current_protocol = 0;
        }

        write_slot_status(
            header.slot,
            header.seq,
            self.current_icc_status(),
            COMMAND_STATUS_NO_ERROR,
            0,
            0,
            response,
        )
    }

    // CCID_SPEC: /* Section 6.1.3 */ struct ccid_pc_to_rdr_get_slot_status {
    // struct ccid_header hdr; uint8_t abRFU[3]; } __attribute__ ((packed));
    // /* Response: RDR_to_PC_SlotStatus */
    fn handle_get_slot_status(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        write_slot_status(
            header.slot,
            header.seq,
            self.current_icc_status(),
            COMMAND_STATUS_NO_ERROR,
            0,
            0,
            response,
        )
    }

    // GEMPC: dwMaxCCIDMessageLength: 271 bytes
    fn handle_xfr_block(&mut self, header: &CcidHeader, apdu: &[u8], response: &mut [u8]) -> usize {
        if P::CONTACT {
            return self.handle_xfr_block_contact(header, apdu, response);
        }
        if self.slot_state != SlotState::PresentActive {
            return write_message(
                RDR_TO_PC_DATABLOCK,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_FAILED, self.current_icc_status()),
                ICC_NOT_ACTIVE,
                0,
                &[],
                response,
            );
        }

        self.apdu_pipeline(header, apdu, response)
    }

    /// Contact-profile XfrBlock, ported byte-for-byte from the STM32
    /// CcidMessageHandler (issue #121): Short-APDU exchange level —
    /// oversize rejection, PPS echo, card relay with contact error
    /// shapes (0xFE inactive / 0xFF card failure, SlotStatus responses).
    fn handle_xfr_block_contact(
        &mut self,
        header: &CcidHeader,
        apdu: &[u8],
        response: &mut [u8],
    ) -> usize {
        if self.slot_state != SlotState::PresentActive {
            return write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_FAILED,
                CCID_ERR_ICC_MUTE,
                0,
                response,
            );
        }

        if apdu.len() > 261 {
            return write_slot_status(
                header.slot,
                header.seq,
                ICC_STATUS_PRESENT_ACTIVE,
                COMMAND_STATUS_FAILED,
                // bError 0x07 (bad DWLength) has no constant in
                // ccid_proto.h — asserted as a literal (quote gap).
                0x07,
                0,
                response,
            );
        }

        if is_pps_request(apdu) {
            return write_message(
                RDR_TO_PC_DATABLOCK,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE),
                0,
                0,
                apdu,
                response,
            );
        }

        match Self::count_transmit(&mut self.diagnostics, &mut self.nfc, apdu, &mut self.tx_buf) {
            Ok(resp_len) => write_message(
                RDR_TO_PC_DATABLOCK,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE),
                0,
                0,
                &self.tx_buf[..resp_len],
                response,
            ),
            Err(e) => {
                // Card exchange failure → bError via the error-map
                // table (`xfr_failure_b_error`, every entry cited):
                // parity surfaces as 0xFD (IFD_PARITY_ERROR on the
                // host), timeout/unclassified as 0xF0. libccid maps a
                // FAILED DataBlock by bError (commands.c CCID_Receive
                // switch, the `case 0xEF`/`0xF0`/`0xFD`/`0xFE` arms):
                // 0xF0 → synthesizes SW 64 00 (clean card error the
                // whole host stack — pcscd, pyscard, GlobalPlatformPro
                // — reports instead of crashing on position(-2)
                // jnasmartcardio bug, gp issue #307); 0xFD →
                // IFD_PARITY_ERROR (the host engine's retry ladder);
                // any other failed code → IFD_COMMUNICATION_ERROR. A
                // SlotStatus here instead of a DataBlock is valid CCID
                // but jnasmartcardio's transmit path 0-byte SW-strip
                // is the documented crash shape — always answer
                // XfrBlock with a DataBlock.
                let b_error = xfr_failure_b_error(self.nfc.xfr_error_class(&e));
                write_data_block(
                    header.slot,
                    header.seq,
                    ICC_STATUS_PRESENT_ACTIVE,
                    COMMAND_STATUS_FAILED,
                    b_error,
                    0,
                    &[],
                    response,
                )
            }
        }
    }

    fn answer_get_uid(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        let Some(uid) = self.nfc.card_uid() else {
            return self.write_sw(header, &[0x63, 0x00], response);
        };
        let uid_len = uid.len();
        self.tx_buf[..uid_len].copy_from_slice(uid);
        self.tx_buf[uid_len] = 0x90;
        self.tx_buf[uid_len + 1] = 0x00;
        // Local reader-generated answer — no card I/O, so no rx counting
        // (Codex review on #81: to/from-card semantics).
        write_message(
            RDR_TO_PC_DATABLOCK,
            header.slot,
            header.seq,
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentActive.icc_status(),
            ),
            0,
            0,
            &self.tx_buf[..uid_len + 2],
            response,
        )
    }

    fn write_sw(&mut self, header: &CcidHeader, sw: &[u8; 2], response: &mut [u8]) -> usize {
        // Local reader-generated status word — no card I/O, no rx counting.
        write_message(
            RDR_TO_PC_DATABLOCK,
            header.slot,
            header.seq,
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentActive.icc_status(),
            ),
            0,
            0,
            sw,
            response,
        )
    }

    fn handle_set_parameters(
        &mut self,
        header: &CcidHeader,
        payload: &[u8],
        response: &mut [u8],
    ) -> usize {
        if P::CONTACT {
            return self.handle_set_parameters_contact(header, payload, response);
        }
        // CCID Rev 1.1 §6.1.7: bProtocolNum 0 = T=0, 1 = T=1; any other
        // value is invalid. Reject with CMD_NOT_SUPPORTED (SlotStatus
        // failed) instead of storing/echoing a bogus protocol that a
        // protocol-dependent route (T=1 endpoint) would then silently
        // fall back from (issue #114, Codex on #93).
        if header.specific[0] > 1 {
            return write_slot_status(
                header.slot,
                header.seq,
                self.current_icc_status(),
                COMMAND_STATUS_FAILED,
                CCID_ERR_CMD_NOT_SUPPORTED,
                0,
                response,
            );
        }
        self.current_protocol = header.specific[0];
        // G-04: store the accepted T=1 structure so GetParameters echoes
        // the applied set (what pcscd believes) instead of the static
        // defaults, and so wrappers can apply the negotiated timing to
        // protocol endpoints via `applied_t1_timing`.
        if header.specific[0] == 1 && header.length == 7 && payload.len() >= 7 {
            let mut raw = [0u8; 7];
            raw.copy_from_slice(&payload[..7]);
            self.host_params = Some((1, raw));
        }
        self.parameters_accepted_event = true;
        self.write_parameters(header, response)
    }

    /// Contact-profile SetParameters, ported byte-for-byte from the STM32
    /// CcidMessageHandler (issue #121): dwLength↔bProtocolNum agreement,
    /// libccid quirk field validation, host-parameter storage + verbatim
    /// echo (the mechanism pcscd relies on to apply negotiated Fi/Di and
    /// BWI/CWI after PPS). The accepted-protocol event fires so the
    /// wrapper can tell the USART driver (`set_protocol`).
    fn handle_set_parameters_contact(
        &mut self,
        header: &CcidHeader,
        payload: &[u8],
        response: &mut [u8],
    ) -> usize {
        // CCID_SPEC: struct ccid_pc_to_rdr_set_parameters { struct ccid_header hdr;
        // uint8_t bProtocolNum; uint8_t abRFU[2]; union { struct ccid_proto_data_t0 t0;
        // struct ccid_proto_data_t1 t1; } abProtocolData; } __attribute__ ((packed));
        let data_len = header.length as usize;

        let length_implied_protocol = match data_len {
            5 => 0,
            7 => 1,
            _ => {
                return write_slot_status(
                    header.slot,
                    header.seq,
                    self.current_icc_status(),
                    COMMAND_STATUS_FAILED,
                    0x07,
                    0,
                    response,
                );
            }
        };

        let mut raw = [0u8; 7];
        let copy_len = data_len.min(raw.len()).min(payload.len());
        raw[..copy_len].copy_from_slice(&payload[..copy_len]);

        // bProtocolNum (header byte 7) must agree with the structure length;
        // bmTCCKST1: only the EDC bit is meaningful; bIFSC: 0x10..=0xFE;
        // bNadValue: shall be 0 (NAD not supported); bmTCCKST0: only the
        // convention bit. bError 0x08 (bad parameter) has no constant in
        // ccid_proto.h (quote gap, asserted as a literal).
        let requested_protocol = header.specific[0];
        let valid = requested_protocol == length_implied_protocol
            && if requested_protocol == 1 {
                (raw[1] & !0x01) == 0 && (16..=254).contains(&raw[5]) && raw[6] == 0
            } else {
                (raw[1] & !0x80) == 0
            };
        if !valid {
            return write_message(
                RDR_TO_PC_PARAMETERS,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_FAILED, self.current_icc_status()),
                0x08,
                0,
                &[],
                response,
            );
        }

        self.current_protocol = requested_protocol;
        self.host_params = Some((requested_protocol, raw));
        self.parameters_accepted_event = true;

        // Keep AtrParams coherent with the accepted set so any consumer of
        // the negotiated values (Fi/Di, BWI/CWI, IFSC, EDC, guard time)
        // sees the host-requested configuration, overriding ATR defaults.
        let p = &mut self.atr_params;
        p.has_ta1 = true;
        p.ta1 = raw[0];
        p.fi = fi_from_ta1_high(raw[0] >> 4);
        p.di = di_from_ta1_low(raw[0] & 0x0F);
        p.guard_time_n = raw[2];
        if requested_protocol == 1 {
            let (bwi, cwi, ifsc) = t1_bwi_cwi_ifsc(&raw);
            p.bwi = bwi;
            p.cwi = cwi;
            p.edc_type = raw[1] & 1;
            p.ifsc = ifsc;
        }

        self.write_parameters(header, response)
    }

    fn handle_reset_parameters(&mut self, header: &CcidHeader, response: &mut [u8]) -> usize {
        if P::CONTACT {
            // Contact default is T=0 with the plain 5-byte structure;
            // host-supplied overrides are dropped (ported from the STM32
            // handler).
            self.atr_params = AtrParams::default();
            self.current_protocol = 0;
            self.host_params = None;
            self.parameters_accepted_event = true;
            return self.write_parameters(header, response);
        }
        // Reset restores T=1 (contactless reader default).
        self.current_protocol = 1;
        self.parameters_accepted_event = true;
        self.write_parameters(header, response)
    }

    /// APDU-level pipeline shared by the direct XfrBlock path and
    /// transport-glue relay paths (the ESP32 T=1 route re-enters it):
    /// pseudo-APDUs (0xFF), PPS echo, card relay. Writes CCID DataBlock
    /// bytes into `out` (10-byte header + payload) and returns the total
    /// length; T=1 callers extract the payload for block framing.
    pub fn apdu_pipeline(&mut self, header: &CcidHeader, apdu: &[u8], out: &mut [u8]) -> usize {
        let response = out;
        if is_pps_request(apdu) {
            log::info!("xfr_block: PPS request, echoing back: {:02X?}", apdu);
            // PPS is handled locally (echoed), not forwarded to the card —
            // per the Diagnostics field docs (APDUs to/from the card) it is
            // deliberately NOT counted in apdu_tx/rx.
            return write_message(
                RDR_TO_PC_DATABLOCK,
                header.slot,
                header.seq,
                build_bstatus(
                    COMMAND_STATUS_NO_ERROR,
                    SlotState::PresentActive.icc_status(),
                ),
                0,
                0,
                apdu,
                response,
            );
        }

        // PC/SC pseudo-APDUs (CLA=0xFF) are answered by the reader itself,
        // never forwarded to the card. CLA=0x00 (ISO 7816) and other CLA
        // values still go to the card — the ACR1252 only intercepts 0xFF.
        // A complete 4-byte header is required; shorter fragments are not
        // routable and keep flowing to the card (issue #49, issue #50).
        // Contact-profile readers (PSEUDO_APDUS=false) forward everything
        // but PPS to the card.
        if P::PSEUDO_APDUS && apdu.first() == Some(&0xFF) && apdu.len() >= 4 {
            return match (apdu[1], apdu[2], apdu[3]) {
                (0xCA, 0x00, 0x00) => self.answer_get_uid(header, response),
                (0xCA, _, _) => self.write_sw(header, &[0x6A, 0x86], response),
                _ => self.write_sw(header, &[0x63, 0x00], response),
            };
        }

        // Counted at the moment of actual card transfer (Codex reviews on
        // #46/#43/#38): rejected/inactive requests and locally echoed PPS
        // frames are not APDUs sent to the card.
        match Self::count_transmit(&mut self.diagnostics, &mut self.nfc, apdu, &mut self.tx_buf) {
            Ok(resp_len) => write_message(
                RDR_TO_PC_DATABLOCK,
                header.slot,
                header.seq,
                build_bstatus(
                    COMMAND_STATUS_NO_ERROR,
                    SlotState::PresentActive.icc_status(),
                ),
                0,
                0,
                &self.tx_buf[..resp_len],
                response,
            ),
            Err(e) => {
                // Card timeout or death mid-exchange — same convention
                // as the contact arm above (#152), routed through the
                // shared error-map table: NFC frontends don't override
                // `xfr_error_class` (ISO 14443 handles block recovery
                // below us), so the class is Unclassified → FAILED
                // DataBlock + bError 0xF0 (card timeout) + honest
                // dwLength=0. libccid's CCID_Receive maps this exact
                // shape to a host-side synthesized SW 64 00 +
                // IFD_SUCCESS (reference/CCID/src/commands.c "case
                // 0xF0: rx_buffer 64 00, *rx_length=2") so direct TPDU
                // consumers get a clean status word instead of empty
                // data — the GlobalPlatformPro ByteBuffer crash class
                // (gp #307). The old bError 0xFB (hw error) fell into
                // CCID_Receive's `default:` → IFD_COMMUNICATION_ERROR.
                // The T=1-armed serial route never exposes this frame
                // (the endpoint re-frames the payload — see
                // ccid_handler.rs #152 synthesis).
                let b_error = xfr_failure_b_error(self.nfc.xfr_error_class(&e));
                self.slot_state = SlotState::PresentInactive;
                write_message(
                    RDR_TO_PC_DATABLOCK,
                    header.slot,
                    header.seq,
                    build_bstatus(COMMAND_STATUS_FAILED, self.current_icc_status()),
                    b_error,
                    0,
                    &[],
                    response,
                )
            }
        }
    }

    // CCID_SPEC: /* Section 6.1.8 */ struct ccid_pc_to_rdr_escape {
    // struct ccid_header hdr; uint8_t abRFU[3]; uint8_t abData[0]; }
    // __attribute__ ((packed)); /* Response: RDR_to_PC_Escape */
    fn handle_escape(&mut self, header: &CcidHeader, payload: &[u8], response: &mut [u8]) -> usize {
        if payload.first() == Some(&0xD0) {
            // Live core counters + the optional driver-known-state fold
            // (#344): the STM32 wrapper's F746 presence-pin overlay,
            // applied at answer time. Default (no fold installed) is
            // byte-identical to the cached snapshot the ESP32 answers
            // with — pinned by tests on both profiles.
            let mut diag = self.diagnostics;
            if let Some(fold) = self.diagnostics_fold {
                fold(&self.nfc, &mut diag);
            }
            let mut diag_buf = [0u8; Diagnostics::SERIALIZED_SIZE];
            diag.to_bytes(&mut diag_buf);
            return write_message(
                RDR_TO_PC_ESCAPE,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
                0,
                0,
                &diag_buf,
                response,
            );
        }

        // The serial/GemPC escape table below (0xD1 snapshot, 0x02
        // firmware identity, 0x01-01-01 sync notifications) is the
        // GemPC-Twin serial surface: the contact profile never answered
        // these pre-port (its wrapper rejected everything but 0xD0 and
        // its Gemalto 0x6A), so they are profile-gated to keep the
        // delegated contact wire byte-identical (track I, #121).
        if !P::CONTACT {
            // Dump-and-retrieve debugging: ack, then the serve loop panics with
            // a marker reason after this response goes out — the panic handler
            // writes the flash coredump (retrieve + decode: AGENTS.md).
            if payload.first() == Some(&0xD1) {
                self.diagnostic_snapshot_requested = true;
                return write_message(
                    RDR_TO_PC_ESCAPE,
                    header.slot,
                    header.seq,
                    build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
                    0,
                    0,
                    &[0xD1],
                    response,
                );
            }

            // CCID_SERIAL: tx_buffer[0] = 0x02; // get reader firmware
            if payload.first() == Some(&0x02) {
                return write_message(
                    RDR_TO_PC_ESCAPE,
                    header.slot,
                    header.seq,
                    build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
                    0,
                    0,
                    self.firmware_version,
                    response,
                );
            }

            // CCID_SERIAL: perform a command to configure GemPC Twin reader card movement
            // * notification to synchronous mode: the card movement is notified _after_
            // * the host command and _before_ the reader answer
            // CCID_SERIAL: unsigned char tx_buffer[] = { 0x01, 0x01, 0x01};
            if payload.starts_with(&[0x01, 0x01, 0x01]) {
                self.sync_notifications = true;
                return write_message(
                    RDR_TO_PC_ESCAPE,
                    header.slot,
                    header.seq,
                    build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
                    0,
                    0,
                    &[0x01, 0x01, 0x01],
                    response,
                );
            }
        }

        // libccid's ccid_open_hack_post() sends Escape{0x1F, 0x02} to
        // GEMPCKEY/GEMPCTWIN readers under DRIVER_OPTION_GEMPC_TWIN_KEY_APDU
        // (reference/CCID/src/ccid.c:299-313) and flips dwFeatures to
        // CCID_CLASS_SHORT_APDU when it succeeds. Rejecting on purpose: this
        // firmware does NOT implement APDU exchange level (T=1 stays
        // firmware-terminated), so a success ack would silently change the
        // host's framing expectations. A failed escape leaves the driver at
        // TPDU level, which is the level we actually speak. Profile-neutral:
        // the contact wrapper rejected it with the identical bytes pre-port.
        if payload == [0x1F, 0x02] {
            return write_message(
                RDR_TO_PC_ESCAPE,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_FAILED, self.current_icc_status()),
                0,
                CCID_ERR_CMD_NOT_SUPPORTED,
                &[],
                response,
            );
        }

        // Unsupported escape payloads answer in the Escape response type, not
        // SlotStatus: this is what osmo-ccid-firmware does
        // (ccid_device.c: resp = ccid_gen_escape(cs, seq,
        // CCID_CMD_STATUS_FAILED, CCID_ERR_CMD_NOT_SUPPORTED, NULL, 0);)
        // and what our own STM32 sibling (send_err_resp, ccid_core.rs) does.
        // libccid's CmdEscapeCheck tolerates either shape, but CCID Rev 1.1
        // §6.1.8 maps PC_to_RDR_Escape to RDR_to_PC_Escape and the sibling
        // divergence is exactly the mock/hardware-divergence bug class the
        // fix-all-siblings protocol exists for (#141 audit, F-1).
        write_message(
            RDR_TO_PC_ESCAPE,
            header.slot,
            header.seq,
            build_bstatus(COMMAND_STATUS_FAILED, self.current_icc_status()),
            0,
            CCID_ERR_CMD_NOT_SUPPORTED,
            &[],
            response,
        )
    }

    fn write_parameters(&self, header: &CcidHeader, response: &mut [u8]) -> usize {
        let (payload, len, protocol) = self.parameter_payload();
        write_message(
            RDR_TO_PC_PARAMETERS,
            header.slot,
            header.seq,
            build_bstatus(COMMAND_STATUS_NO_ERROR, self.current_icc_status()),
            0,
            protocol,
            &payload[..len],
            response,
        )
    }

    fn current_icc_status(&self) -> u8 {
        self.slot_state.icc_status()
    }

    /// Current abProtocolDataStructure: `(buffer, length, bProtocolNum)`.
    /// Serial profile: the stored host-supplied set when it matches the
    /// active protocol (the accepted SetParameters echo — G-04), else the
    /// static defaults. Contact profile: the stored host-supplied set
    /// when it matches the active protocol, else the ATR-derived values
    /// (ported from the STM32 handler, issue #121).
    fn parameter_payload(&self) -> ([u8; 7], usize, u8) {
        if !P::CONTACT {
            if let Some((proto, raw)) = self.host_params {
                if proto == self.current_protocol && proto == 1 {
                    return (raw, 7, proto);
                }
            }
            let defaults = default_params(self.current_protocol);
            let mut buf = [0u8; 7];
            let len = defaults.len().min(7);
            buf[..len].copy_from_slice(&defaults[..len]);
            return (buf, len, self.current_protocol);
        }
        if let Some((proto, raw)) = self.host_params {
            if proto == self.current_protocol {
                let len = if proto == 1 { 7 } else { 5 };
                return (raw, len, proto);
            }
        }
        let p = &self.atr_params;
        let params: [u8; 7] = if self.current_protocol == 1 {
            [
                if p.has_ta1 { p.ta1 } else { 0x11 },
                (p.edc_type & 1) << 4,
                p.guard_time_n,
                (p.bwi << 4) | (p.cwi & 0x0F),
                0x00,
                p.ifsc.min(254),
                0x00,
            ]
        } else {
            [
                if p.has_ta1 { p.ta1 } else { 0x11 },
                0x00,
                p.guard_time_n,
                // T=0 waiting integer (plain WI, 0x00 default) — not the
                // T=1 (BWI<<4)|CWI packing.
                0x00,
                0x00,
                0x00,
                0x00,
            ]
        };
        let len = if self.current_protocol == 1 { 7 } else { 5 };
        (params, len, self.current_protocol)
    }

    // CCID_SPEC: /* Section 6.1.11 */ struct ccid_pc_to_rdr_secure {
    // struct ccid_header hdr; uint8_t bBWI; uint16_t wLevelParameter;
    // uint8_t abData[0]; } __attribute__ ((packed));
    // struct ccid_pin_operation_data { uint8_t bPINOperation;
    // uint8_t abPNDataStructure[0]; } __attribute__ ((packed));

    /// PC_to_RDR_Secure (PIN verify/modify), ported from the STM32
    /// handler (issue #121). Non-pin-pad readers get CMD_NOT_SUPPORTED in
    /// their profile's shape (DataBlock for contact — pinned by the
    /// firmware suite; SlotStatus for the serial profile, matching the
    /// pre-port unknown-command arm). A successful parse arms the secure
    /// session and returns 0 — the response is deferred until
    /// [`Self::complete_pin_entry`].
    fn handle_secure(&mut self, header: &CcidHeader, payload: &[u8], response: &mut [u8]) -> usize {
        let secure_not_supported = |this: &Self, response: &mut [u8]| -> usize {
            if P::CONTACT {
                write_message(
                    RDR_TO_PC_DATABLOCK,
                    header.slot,
                    header.seq,
                    build_bstatus(COMMAND_STATUS_FAILED, this.current_icc_status()),
                    CCID_ERR_CMD_NOT_SUPPORTED,
                    0,
                    &[],
                    response,
                )
            } else {
                write_slot_status(
                    header.slot,
                    header.seq,
                    this.current_icc_status(),
                    COMMAND_STATUS_FAILED,
                    CCID_ERR_CMD_NOT_SUPPORTED,
                    0,
                    response,
                )
            }
        };

        if !self.pin_pad {
            return secure_not_supported(self, response);
        }

        if self.slot_state != SlotState::PresentActive {
            return write_message(
                RDR_TO_PC_DATABLOCK,
                header.slot,
                header.seq,
                build_bstatus(COMMAND_STATUS_FAILED, self.current_icc_status()),
                CCID_ERR_CMD_SLOT_BUSY,
                0,
                &[],
                response,
            );
        }

        let Some(pin_operation) = payload.first().copied() else {
            return secure_not_supported(self, response);
        };

        let pin_data = &payload[1..];

        match pin_operation {
            0x00 => match PinVerifyParams::parse(pin_data) {
                Some(params) => {
                    self.secure_state = SecureState::WaitingForPinVerify {
                        seq: header.seq,
                        params,
                    };
                    // PIN entry is asynchronous: no response is written now;
                    // the wrapper completes the exchange after pin-pad
                    // entry. Length 0 = "deferred".
                    0
                }
                None => secure_not_supported(self, response),
            },
            0x01 => match PinModifyParams::parse(pin_data) {
                Some(params) => {
                    self.secure_state = SecureState::WaitingForPinModify {
                        seq: header.seq,
                        params,
                    };
                    0
                }
                None => secure_not_supported(self, response),
            },
            _ => secure_not_supported(self, response),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use card_interface::{CardBackend, NfcCardExt};
    use ccid_protocol::types::{DEFAULT_T0_PARAMS, DEFAULT_T1_PARAMS, RDR_TO_PC_SLOTSTATUS};
    use std::vec::Vec;

    const ATR: [u8; 5] = [0x3B, 0x80, 0x80, 0x01, 0x01];
    const APDU_RESPONSE: [u8; 2] = [0x90, 0x00];
    const FIRMWARE_VERSION: &[u8] = b"core-test-reader 0.1\0";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum TestError {
        NoCard,
        NotInitialized,
        BufferOverflow,
    }

    /// Mirrors the ESP32 `MockNfcDriver` semantics against the shared
    /// card-interface traits.
    #[derive(Debug, Clone)]
    struct TestDriver {
        card_present: bool,
        atr: Vec<u8>,
        apdu_response: Vec<u8>,
        session_active: bool,
        uid: Vec<u8>,
        fail_power_on: bool,
    }

    impl TestDriver {
        fn new(card_present: bool, atr: &[u8], apdu_response: &[u8]) -> Self {
            Self {
                card_present,
                atr: atr.to_vec(),
                apdu_response: apdu_response.to_vec(),
                session_active: false,
                uid: Vec::new(),
                fail_power_on: false,
            }
        }

        fn set_uid(&mut self, uid: &[u8]) {
            self.uid = uid.to_vec();
        }

        fn set_card_present(&mut self, present: bool) {
            self.card_present = present;
            if !present {
                self.session_active = false;
            }
        }
    }

    impl CardBackend for TestDriver {
        type Error = TestError;

        fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
            if self.fail_power_on {
                return Err(TestError::NoCard);
            }
            if !self.card_present {
                self.session_active = false;
                return Err(TestError::NoCard);
            }
            if atr_buf.len() < self.atr.len() {
                return Err(TestError::BufferOverflow);
            }
            atr_buf[..self.atr.len()].copy_from_slice(&self.atr);
            self.session_active = true;
            Ok(self.atr.len())
        }

        fn power_off(&mut self) {
            self.session_active = false;
        }

        fn is_card_present(&mut self) -> bool {
            self.card_present
        }

        fn transmit_apdu(
            &mut self,
            _command: &[u8],
            response: &mut [u8],
        ) -> Result<usize, Self::Error> {
            if !self.card_present {
                self.session_active = false;
                return Err(TestError::NoCard);
            }
            if !self.session_active {
                return Err(TestError::NotInitialized);
            }
            if response.len() < self.apdu_response.len() {
                return Err(TestError::BufferOverflow);
            }
            response[..self.apdu_response.len()].copy_from_slice(&self.apdu_response);
            Ok(self.apdu_response.len())
        }
    }

    impl NfcCardExt for TestDriver {
        fn init(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_card_presence(&mut self) -> PresenceState {
            if !self.card_present {
                self.session_active = false;
            }
            PresenceState {
                present: self.card_present,
            }
        }

        fn session_active(&self) -> bool {
            self.session_active
        }

        fn card_uid(&self) -> Option<&[u8]> {
            (!self.uid.is_empty()).then_some(&self.uid)
        }
    }

    fn build_ccid_cmd(msg_type: u8, slot: u8, seq: u8, payload: &[u8]) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.push(msg_type);
        let len = payload.len() as u32;
        msg.extend_from_slice(&len.to_le_bytes());
        msg.push(slot);
        msg.push(seq);
        msg.push(0x00);
        msg.push(0x00);
        msg.push(0x00);
        msg.extend_from_slice(payload);
        msg
    }

    fn build_set_parameters_cmd(slot: u8, seq: u8, protocol: u8, payload: &[u8]) -> Vec<u8> {
        let mut msg = build_ccid_cmd(PC_TO_RDR_SET_PARAMETERS, slot, seq, payload);
        msg[7] = protocol;
        msg
    }

    fn parse_response(bytes: &[u8]) -> (CcidHeader, &[u8]) {
        let header = CcidHeader::parse(bytes).expect("response header");
        let payload_len = header.length as usize;
        (
            header.clone(),
            &bytes[CCID_HEADER_SIZE..CCID_HEADER_SIZE + payload_len],
        )
    }

    fn new_handler(card_present: bool) -> CcidHandlerCore<TestDriver> {
        CcidHandlerCore::new(
            TestDriver::new(card_present, &ATR, &APDU_RESPONSE),
            FIRMWARE_VERSION,
        )
    }

    #[test]
    fn test_power_on_with_card_returns_atr() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 7, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(header.slot, 0);
        assert_eq!(header.seq, 7);
        assert_eq!(
            header.specific,
            [
                build_bstatus(
                    COMMAND_STATUS_NO_ERROR,
                    SlotState::PresentActive.icc_status()
                ),
                0,
                0
            ]
        );
        assert_eq!(payload, ATR);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);
    }

    #[test]
    fn test_power_on_without_card_returns_slot_status_error() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 1, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC)
        );
        assert_eq!(header.specific[1], CCID_ERR_CMD_NOT_SUPPORTED);
        assert_eq!(handler.slot_state(), SlotState::Absent);
    }

    #[test]
    fn test_power_on_success_signals_card_reset_exactly_once() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let mut response = [0u8; 271];

        assert!(!handler.take_card_reset());
        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&cmd, &mut response);
        assert!(handler.take_card_reset());
        assert!(!handler.take_card_reset());
    }

    #[test]
    fn test_power_on_failure_does_not_signal_card_reset() {
        // Present-but-activation-fails (the real PN7160 bench case): the
        // event must fire only on SUCCESSFUL resets, since the wrapper
        // uses it to disarm protocol routes.
        let mut handler = new_handler(true);
        handler.check_card_change();
        handler.driver_mut().fail_power_on = true;
        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        let mut response = [0u8; 271];

        handler.process_command(&cmd, &mut response);
        assert!(!handler.take_card_reset());
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
    }

    #[test]
    fn test_power_off_returns_present_inactive() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 2, &[]);
        handler.process_command(&power_on, &mut response);

        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_OFF, 0, 3, &[]);
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
    }

    #[test]
    fn test_get_slot_status_with_card_reports_present() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let cmd = build_ccid_cmd(PC_TO_RDR_GET_SLOT_STATUS, 0, 4, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, _) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
    }

    #[test]
    fn test_get_slot_status_without_card_reports_not_present() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_GET_SLOT_STATUS, 0, 5, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, _) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_NO_ICC)
        );
        assert_eq!(handler.slot_state(), SlotState::Absent);
    }

    #[test]
    fn test_xfr_block_succeeds_when_card_is_active() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 7, &[0x00, 0xA4, 0x04, 0x00, 0x00]);
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        // CCID Rev 1.1 §6.2-1: the device shall echo bSeq of the
        // corresponding PC_to_RDR command. The pipeline once fabricated a
        // zeroed header here — every card-bound XfrBlock answered bSeq=0
        // (invisible to pcscd, fatal to strict parsers; bench 2026-10-11).
        assert_eq!(header.seq, 7);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentActive.icc_status()
            )
        );
        assert_eq!(payload, APDU_RESPONSE);
    }

    #[test]
    fn test_xfr_block_when_not_powered_returns_icc_not_active() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 8, &[0x00, 0x84, 0x00, 0x00]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_FAILED,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(header.specific[1], ICC_NOT_ACTIVE);
    }

    #[test]
    fn test_escape_firmware_returns_injected_version_string() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 9, &[0x02]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_NO_ERROR, ICC_STATUS_NO_ICC)
        );
        assert_eq!(payload, FIRMWARE_VERSION);
    }

    #[test]
    fn test_escape_snapshot_arms_exactly_once() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 9, &[0xD1]);
        let mut response = [0u8; 271];

        assert!(!handler.take_snapshot_request());

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert_eq!(payload, &[0xD1]);
        assert!(handler.take_snapshot_request());
        assert!(!handler.take_snapshot_request());
    }

    #[test]
    fn test_escape_sync_notification_enables_sync_mode() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 10, &[0x01, 0x01, 0x01]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert_eq!(payload, [0x01, 0x01, 0x01]);
        assert!(handler.sync_notifications());
    }

    #[test]
    fn test_escape_unknown_payload_answers_escape_type() {
        // osmo-ccid-firmware answers failed escapes with an Escape frame
        // (ccid_device.c: ccid_gen_escape(..., CCID_CMD_STATUS_FAILED,
        // CCID_ERR_CMD_NOT_SUPPORTED, NULL, 0)) — not SlotStatus (#141 F-1)
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 10, &[0xFF]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC)
        );
        assert_eq!(header.specific[1], CCID_ERR_CMD_NOT_SUPPORTED);
    }

    #[test]
    fn test_escape_apdu_level_switch_rejected() {
        // libccid's ccid_open_hack_post() flips dwFeatures to SHORT_APDU
        // when Escape{0x1F,0x02} succeeds (reference/CCID/src/ccid.c,
        // GEMPCKEY/GEMPCTWIN under DRIVER_OPTION_GEMPC_TWIN_KEY_APDU);
        // this firmware speaks TPDU level only, so the switch must fail
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(PC_TO_RDR_ESCAPE, 0, 11, &[0x1F, 0x02]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_ESCAPE);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC)
        );
        assert_eq!(header.specific[1], CCID_ERR_CMD_NOT_SUPPORTED);
    }

    #[test]
    fn test_unknown_command_returns_cmd_not_supported() {
        let mut handler = new_handler(false);
        let cmd = build_ccid_cmd(0x71, 0, 11, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC)
        );
        assert_eq!(header.specific[1], CCID_ERR_CMD_NOT_SUPPORTED);
    }

    #[test]
    fn test_check_card_change_detects_insertion_and_removal() {
        let mut handler = new_handler(false);

        assert_eq!(handler.check_card_change(), None);

        handler.driver_mut().set_card_present(true);
        assert_eq!(handler.check_card_change(), Some(true));

        // full activation, then removal
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 30, &[]);
        let mut response = [0u8; 271];
        handler.process_command(&power_on, &mut response);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);

        handler.driver_mut().set_card_present(false);
        assert_eq!(handler.check_card_change(), Some(false));
        assert_eq!(handler.slot_state(), SlotState::Absent);
    }

    #[test]
    fn test_session_lifecycle() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 16, &[]);
        let power_on_len = handler.process_command(&power_on, &mut response);
        let (power_on_header, power_on_payload) = parse_response(&response[..power_on_len]);

        assert_eq!(power_on_header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(power_on_payload, ATR);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);
        assert!(NfcCardExt::session_active(handler.driver_mut()));

        let xfr = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 17, &[0x00, 0x84, 0x00, 0x00]);
        let xfr_len = handler.process_command(&xfr, &mut response);
        let (xfr_header, xfr_payload) = parse_response(&response[..xfr_len]);

        assert_eq!(xfr_header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(xfr_payload, APDU_RESPONSE);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);
        assert!(NfcCardExt::session_active(handler.driver_mut()));

        let power_off = build_ccid_cmd(PC_TO_RDR_ICC_POWER_OFF, 0, 18, &[]);
        let power_off_len = handler.process_command(&power_off, &mut response);
        let (power_off_header, power_off_payload) = parse_response(&response[..power_off_len]);

        assert_eq!(power_off_header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert!(power_off_payload.is_empty());
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
        assert!(!NfcCardExt::session_active(handler.driver_mut()));
    }

    #[test]
    fn test_poll_skips_when_session_active() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);

        assert_eq!(handler.check_card_change(), None);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);
        assert!(handler.presence_state().present);
    }

    #[test]
    fn test_apdu_failure_downgrades_to_present_inactive_when_card_remains_present() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);
        handler.driver_mut().power_off();

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 7, &[0x00, 0xA4, 0x04, 0x00]);
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_FAILED,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
    }

    // GREAT EXPECTATION (#152): the NFC/serial relay-failure frame is
    // byte-locked here — DataBlock, FAILED + post-failure ICC status,
    // bError 0xF0 (card timeout, NOT hw error), dwLength honestly 0.
    // Convention: libccid CCID_Receive "case 0xF0" synthesizes SW 64 00
    // host-side (reference/CCID/src/commands.c) — sibling of the contact
    // arm's xfr_block_relay_failure_answers_datablock_timeout_sw_and_
    // stays_active golden.
    #[test]
    fn xfr_block_relay_failure_nfc_answers_datablock_card_timeout_error() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);
        handler.driver_mut().set_card_present(false);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 7, &[0x00, 0xA4, 0x04, 0x00]);
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_FAILED,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(header.specific[1], CCID_ERR_CARD_TIMEOUT);
        assert!(payload.is_empty());
        // dwLength honesty is implied by the empty payload parse; lock
        // the wire bytes explicitly (header layout: type, dwLength LE,
        // slot, seq, bStatus, bError, bSpecific).
        assert_eq!(response[0], RDR_TO_PC_DATABLOCK);
        assert_eq!(u32::from_le_bytes(response[1..5].try_into().unwrap()), 0);
        assert_eq!(response[5], 0);
        assert_eq!(response[6], 7);
        assert_eq!(response[8], CCID_ERR_CARD_TIMEOUT);
        assert_eq!(len, 10);
        assert_eq!(handler.slot_state(), SlotState::PresentInactive);
    }

    #[test]
    fn test_get_parameters_returns_default_t1_params() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let cmd = build_ccid_cmd(PC_TO_RDR_GET_PARAMETERS, 0, 12, &[]);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_PARAMETERS);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentInactive.icc_status()
            )
        );
        assert_eq!(header.specific[2], 1);
        assert_eq!(payload, DEFAULT_T1_PARAMS);
    }

    #[test]
    fn test_set_parameters_updates_protocol_and_returns_t0_params() {
        let mut handler = new_handler(true);
        let cmd = build_set_parameters_cmd(0, 13, 0, &DEFAULT_T0_PARAMS);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_PARAMETERS);
        assert_eq!(header.specific[2], 0);
        assert_eq!(payload, DEFAULT_T0_PARAMS);
        assert_eq!(handler.current_protocol(), 0);
        assert!(handler.take_parameters_accepted());
    }

    #[test]
    fn test_set_parameters_rejects_invalid_protocol_num() {
        let mut handler = new_handler(true);
        let cmd = build_set_parameters_cmd(0, 21, 2, &DEFAULT_T1_PARAMS);
        let mut response = [0u8; 271];

        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(header.seq, 21);
        assert!(payload.is_empty());
        assert_eq!(
            header.specific[0],
            build_bstatus(COMMAND_STATUS_FAILED, ICC_STATUS_NO_ICC)
        );
        assert_eq!(header.specific[1], CCID_ERR_CMD_NOT_SUPPORTED);
        assert_eq!(handler.current_protocol(), 1);
        assert!(!handler.take_parameters_accepted());
    }

    #[test]
    fn test_reset_parameters_restores_t1_defaults() {
        let mut handler = new_handler(true);
        let set_cmd = build_set_parameters_cmd(0, 14, 0, &DEFAULT_T0_PARAMS);
        let mut response = [0u8; 271];
        handler.process_command(&set_cmd, &mut response);
        assert!(handler.take_parameters_accepted());

        let reset_cmd = build_ccid_cmd(PC_TO_RDR_RESET_PARAMETERS, 0, 15, &[]);
        let len = handler.process_command(&reset_cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_PARAMETERS);
        assert_eq!(header.specific[2], 1);
        assert_eq!(payload, DEFAULT_T1_PARAMS);
        assert_eq!(handler.current_protocol(), 1);
        assert!(handler.take_parameters_accepted());
    }

    #[test]
    fn test_xfr_block_increments_apdu_tx_count() {
        let mut handler = new_handler(true);
        handler.check_card_change();
        let mut response = [0u8; 271];
        let power_on = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 6, &[]);
        handler.process_command(&power_on, &mut response);

        assert_eq!(handler.diagnostics().apdu_tx_count, 0);
        assert_eq!(handler.diagnostics().apdu_rx_count, 0);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 7, &[0x00, 0xA4, 0x04, 0x00]);
        handler.process_command(&cmd, &mut response);

        assert_eq!(handler.diagnostics().apdu_tx_count, 1);
        assert_eq!(handler.diagnostics().apdu_rx_count, 1);
    }

    #[test]
    fn test_error_response_increments_error_count() {
        let mut handler = new_handler(false);
        let mut response = [0u8; 271];

        assert_eq!(handler.diagnostics().error_count, 0);

        let cmd = build_ccid_cmd(0x71, 0, 11, &[]);
        handler.process_command(&cmd, &mut response);

        assert_eq!(handler.diagnostics().error_count, 1);
    }

    #[test]
    fn test_card_present_reflected_in_diagnostics() {
        let mut handler = new_handler(false);
        handler.check_card_change();
        assert!(!handler.diagnostics().card_present);

        handler.driver_mut().set_card_present(true);
        handler.check_card_change();
        assert!(handler.diagnostics().card_present);

        handler.driver_mut().set_card_present(false);
        handler.check_card_change();
        assert!(!handler.diagnostics().card_present);
    }

    const CARD_UID: [u8; 7] = [0x04, 0xC4, 0x74, 0xFA, 0x96, 0x73, 0x80];

    fn new_handler_with_uid(card_present: bool) -> CcidHandlerCore<TestDriver> {
        let mut nfc = TestDriver::new(card_present, &ATR, &APDU_RESPONSE);
        nfc.set_uid(&CARD_UID);
        CcidHandlerCore::new(nfc, FIRMWARE_VERSION)
    }

    fn power_on(handler: &mut CcidHandlerCore<TestDriver>) {
        handler.check_card_change();
        let cmd = build_ccid_cmd(PC_TO_RDR_ICC_POWER_ON, 0, 30, &[]);
        let mut response = [0u8; 271];
        handler.process_command(&cmd, &mut response);
        assert_eq!(handler.slot_state(), SlotState::PresentActive);
    }

    #[test]
    fn test_get_uid_pseudo_apdu_returns_uid_and_9000() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 31, &[0xFF, 0xCA, 0x00, 0x00, 0x00]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_NO_ERROR,
                SlotState::PresentActive.icc_status()
            )
        );
        let mut expected = CARD_UID.to_vec();
        expected.extend_from_slice(&[0x90, 0x00]);
        assert_eq!(payload, expected.as_slice());
    }

    #[test]
    fn test_get_uid_bad_p1_p2_returns_6a86() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 32, &[0xFF, 0xCA, 0x01, 0x00, 0x00]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (_, payload) = parse_response(&response[..len]);

        assert_eq!(payload, [0x6A, 0x86]);
    }

    #[test]
    fn test_ff_cla_unsupported_ins_returns_6300() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);

        let cmd = build_ccid_cmd(
            PC_TO_RDR_XFR_BLOCK,
            0,
            33,
            &[0xFF, 0xA4, 0x00, 0x0C, 0x02, 0x3F, 0x00],
        );
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (_, payload) = parse_response(&response[..len]);

        assert_eq!(payload, [0x63, 0x00]);
    }

    #[test]
    fn test_short_ff_fragment_still_forwarded_to_card() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 34, &[0xFF]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (_, payload) = parse_response(&response[..len]);

        assert_eq!(payload, APDU_RESPONSE);
    }

    #[test]
    fn test_ff_pps_request_not_intercepted_as_pseudo_apdu() {
        let mut handler = new_handler_with_uid(true);
        power_on(&mut handler);

        let pps = [0xFF, 0x11, 0x00];
        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 35, &pps);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (_, payload) = parse_response(&response[..len]);

        assert_eq!(payload, pps);
    }

    #[test]
    fn test_get_uid_without_cached_uid_returns_6300() {
        let mut handler = new_handler(true);
        power_on(&mut handler);

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 36, &[0xFF, 0xCA, 0x00, 0x00, 0x00]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (_, payload) = parse_response(&response[..len]);

        assert_eq!(payload, [0x63, 0x00]);
    }

    #[test]
    fn test_get_uid_requires_active_slot() {
        let mut handler = new_handler_with_uid(true);
        handler.check_card_change();

        let cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 37, &[0xFF, 0xCA, 0x00, 0x00, 0x00]);
        let mut response = [0u8; 271];
        let len = handler.process_command(&cmd, &mut response);
        let (header, payload) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_DATABLOCK);
        assert!(payload.is_empty());
        assert_eq!(header.specific[1], ICC_NOT_ACTIVE);
    }

    #[test]
    fn test_truncated_input_counts_single_error_and_answers_failed_status() {
        let mut handler = new_handler(true);
        handler.check_card_change();

        assert_eq!(handler.diagnostics().error_count, 0);

        let mut cmd = build_ccid_cmd(PC_TO_RDR_XFR_BLOCK, 0, 40, &[0x00, 0xA4]);
        cmd[1] = 0xFF; // claim 255 bytes, deliver 2
        let mut response = [0u8; 512];
        let len = handler.process_command(&cmd, &mut response);
        let (header, _) = parse_response(&response[..len]);

        assert_eq!(header.message_type, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(
            header.specific[0],
            build_bstatus(
                COMMAND_STATUS_FAILED,
                SlotState::PresentInactive.icc_status()
            )
        );
        // exactly one count: the truncation guard (early return bypasses
        // the post-dispatch accounting)
        assert_eq!(handler.diagnostics().error_count, 1);
    }
}

#[cfg(test)]
mod contact_profile_tests {
    //! Contact-profile (STM32 flavor) wire behavior, ported from the
    //! firmware suites (issue #121): every assertion here pins bytes the
    //! pre-port `CcidMessageHandler` produced, so the thin firmware
    //! wrapper inherits them from one place.

    use super::*;
    use card_interface::{CardBackend, SlotDriver};
    use ccid_protocol::types::{
        CCID_ERR_CMD_NOT_SUPPORTED, PC_TO_RDR_ABORT, PC_TO_RDR_GET_PARAMETERS,
        PC_TO_RDR_RESET_PARAMETERS, PC_TO_RDR_SECURE, PC_TO_RDR_SET_PARAMETERS,
        PC_TO_RDR_XFR_BLOCK, RDR_TO_PC_SLOTSTATUS,
    };
    use std::vec::Vec;

    /// Contact-slot mock: card-detect flag + ATR + APDU answers,
    /// mirroring the firmware `MockSmartcardDriver` semantics.
    #[derive(Debug, Clone)]
    struct ContactDriver {
        present: bool,
        atr: Vec<u8>,
        apdu_response: Vec<u8>,
        power_off_count: u32,
        fail_transmit: bool,
        /// #136: what `xfr_error_class` answers for transmit errors.
        transmit_class: XfrErrorKind,
        /// Track I exchange tests: the exact APDU bytes the card saw.
        last_apdu: Vec<u8>,
        /// #344 fold tests: driver-known presence, fresher than the
        /// polled cache (the F746 presence-pin seam).
        diag_card_present: Option<bool>,
    }

    impl ContactDriver {
        fn new(present: bool, atr: &[u8]) -> Self {
            Self {
                present,
                atr: atr.to_vec(),
                apdu_response: Vec::from([0x90u8, 0x00]),
                power_off_count: 0,
                fail_transmit: false,
                transmit_class: XfrErrorKind::Unclassified,
                last_apdu: Vec::new(),
                diag_card_present: None,
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ContactError {
        NoCard,
        Transmit,
    }

    impl CardBackend for ContactDriver {
        type Error = ContactError;

        fn power_on(&mut self, atr_buf: &mut [u8]) -> Result<usize, Self::Error> {
            if !self.present {
                return Err(ContactError::NoCard);
            }
            atr_buf[..self.atr.len()].copy_from_slice(&self.atr);
            Ok(self.atr.len())
        }

        fn power_off(&mut self) {
            self.power_off_count += 1;
        }

        fn is_card_present(&mut self) -> bool {
            self.present
        }

        fn transmit_apdu(
            &mut self,
            command: &[u8],
            response: &mut [u8],
        ) -> Result<usize, Self::Error> {
            if self.fail_transmit {
                return Err(ContactError::Transmit);
            }
            self.last_apdu.clear();
            self.last_apdu.extend_from_slice(command);
            response[..self.apdu_response.len()].copy_from_slice(&self.apdu_response);
            Ok(self.apdu_response.len())
        }

        fn xfr_error_class(&self, _err: &Self::Error) -> XfrErrorKind {
            self.transmit_class
        }
    }

    /// Contact-slot SlotDriver: presence = card-detect GPIO, no session
    /// short-circuit (the core polls every check).
    impl SlotDriver for ContactDriver {
        fn poll_card_presence(&mut self) -> PresenceState {
            PresenceState {
                present: self.present,
            }
        }

        fn session_active(&self) -> bool {
            false
        }
    }

    /// T=1 ATR with TB2 = BWI3/CWI7 (same probe as ccid_spec_paths).
    const T1_ATR_BWI3_CWI7: [u8; 4] = [0x3B, 0x80, 0x21, 0x37];
    const T0_ATR: [u8; 2] = [0x3B, 0x00];

    type Core = CcidHandlerCore<ContactDriver, ContactProfile>;

    fn new_core(present: bool, atr: &[u8], pin_pad: bool) -> Core {
        let mut core =
            CcidHandlerCore::with_profile(ContactDriver::new(present, atr), b"core", pin_pad);
        // Wrapper's construction-time presence sync (the firmware handler
        // sampled is_card_present() in new()).
        core.check_card_change();
        core
    }

    fn build(msg_type: u8, seq: u8, specific: [u8; 3], payload: &[u8]) -> Vec<u8> {
        let mut msg = Vec::with_capacity(CCID_HEADER_SIZE + payload.len());
        msg.push(msg_type);
        msg.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        msg.push(0);
        msg.push(seq);
        msg.extend_from_slice(&specific);
        msg.extend_from_slice(payload);
        msg
    }

    fn parse(resp: &[u8]) -> (u8, u8, u8, u8, &[u8]) {
        (
            resp[0],
            resp[6],
            resp[7],
            resp[8],
            &resp[CCID_HEADER_SIZE..],
        )
    }

    fn power_on(core: &mut Core, seq: u8) -> usize {
        core.process_command(&build(0x62, seq, [0x00, 0, 0], &[]), &mut [0u8; 271])
    }

    #[test]
    fn power_on_no_card_answers_slot_status_icc_mute() {
        let mut core = new_core(false, &T0_ATR, false);
        let mut resp = [0u8; 271];
        let len = core.process_command(&build(0x62, 1, [0x00, 0, 0], &[]), &mut resp);
        let (mt, _seq, status, err, _) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(status, COMMAND_STATUS_FAILED << 6 | ICC_STATUS_NO_ICC);
        assert_eq!(err, CCID_ERR_ICC_MUTE);
    }

    #[test]
    fn power_on_nonzero_dwlength_and_low_voltage_rejected() {
        let mut core = new_core(true, &T0_ATR, false);
        let mut resp = [0u8; 271];
        let len = core.process_command(&build(0x62, 1, [0x00, 0, 0], &[0xAA]), &mut resp);
        let (mt, _, status, err, _) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(status & 0xC0, COMMAND_STATUS_FAILED << 6);
        assert_eq!(err, CCID_ERR_CMD_NOT_SUPPORTED);

        let len = core.process_command(&build(0x62, 2, [0x02, 0, 0], &[]), &mut resp);
        let (_, _, status, err, _) = parse(&resp[..len]);
        assert_eq!(status & 0x03, ICC_STATUS_PRESENT_INACTIVE);
        assert_eq!(err, CCID_ERR_CMD_NOT_SUPPORTED);
    }

    #[test]
    fn power_on_success_derives_protocol_from_atr() {
        let mut core = new_core(true, &T1_ATR_BWI3_CWI7, false);
        let mut resp = [0u8; 271];
        let len = core.process_command(&build(0x62, 1, [0x00, 0, 0], &[]), &mut resp);
        let (mt, _, status, err, data) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_DATABLOCK);
        assert_eq!(
            status,
            COMMAND_STATUS_NO_ERROR << 6 | ICC_STATUS_PRESENT_ACTIVE
        );
        assert_eq!(err, 0);
        assert_eq!(data, T1_ATR_BWI3_CWI7);
        assert_eq!(core.current_protocol(), 1);
    }

    #[test]
    fn power_off_resets_protocol_to_t0() {
        let mut core = new_core(true, &T1_ATR_BWI3_CWI7, false);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let len = core.process_command(&build(0x63, 2, [0; 3], &[]), &mut resp);
        let (mt, _, status, _, _) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(status & 0x03, ICC_STATUS_PRESENT_INACTIVE);
        assert_eq!(core.current_protocol(), 0);
    }

    #[test]
    fn get_parameters_returns_atr_derived_t1_values() {
        let mut core = new_core(true, &T1_ATR_BWI3_CWI7, false);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let len = core.process_command(&build(PC_TO_RDR_GET_PARAMETERS, 2, [0; 3], &[]), &mut resp);
        let (mt, _, _, _, data) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_PARAMETERS);
        assert_eq!(resp[9], 1);
        assert_eq!(data.len(), 7);
        assert_eq!(data[3], 0x37);
        assert_eq!(data[5], 32);
    }

    #[test]
    fn set_parameters_echoes_host_values_and_fires_event() {
        let mut core = new_core(true, &T1_ATR_BWI3_CWI7, false);
        power_on(&mut core, 1);
        let host_params: [u8; 7] = [0x11, 0x00, 0x00, 0x55, 0x00, 0xFE, 0x00];
        let mut resp = [0u8; 271];
        let len = core.process_command(
            &build(PC_TO_RDR_SET_PARAMETERS, 3, [1, 0, 0], &host_params),
            &mut resp,
        );
        let (mt, _, status, err, data) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_PARAMETERS);
        assert_eq!(status & 0xC0, 0);
        assert_eq!(err, 0);
        assert_eq!(data, host_params);
        assert!(core.take_parameters_accepted());
    }

    /// #122 AUTO_BAUD plumbing: the accepted SetParameters' wire timing
    /// must be decodable by the transport — Fi/Di from byte 0, guard
    /// time N from byte 2, protocol from bProtocolNum. This is exactly
    /// what libccid sends after IccPowerOn when dwFeatures bit 7 makes
    /// it skip its own PPS (ifdhandler.c:982) while still computing
    /// PPS1 from the ATR TA1 (ifdhandler.c:885-899, applied at :1044).
    #[test]
    fn accepted_contact_timing_decodes_host_fi_di_and_guard_time() {
        let mut core = new_core(true, &T1_ATR_BWI3_CWI7, false);
        power_on(&mut core, 1);
        assert!(
            core.accepted_contact_timing().is_none(),
            "no timing before a SetParameters acceptance"
        );

        // libccid's shape for a TA1=0x18 card: Fi/Di=0x18 (Fi=372,
        // Di=8), GT=2, BWI/CWI echo, IFSC=0xFE, NAD=0. (TCCKS here is
        // 0x00 per our pinned validation — NOTE: libccid's own table
        // starts param[1] at 0x10 (ifdhandler.c:1030) which our
        // bmTCCKST1 check rejects with bError 0x08; tracked as a #122
        // follow-up because it gates whether the AUTO_BAUD application
        // ever fires against the real host.)
        let host_params: [u8; 7] = [0x18, 0x00, 0x02, 0x55, 0x00, 0xFE, 0x00];
        let mut resp = [0u8; 271];
        core.process_command(
            &build(PC_TO_RDR_SET_PARAMETERS, 3, [1, 0, 0], &host_params),
            &mut resp,
        );
        assert_eq!(
            core.accepted_contact_timing(),
            Some(ContactWireTiming {
                fi: 372,
                di: 12, // TA1 0x18: high nibble 1 → Fi=372, code 8 → Di=12
                guard_time_n: 2,
                protocol: 1,
            })
        );

        // T=0 shape: 5-byte structure — Fi/Di byte 0, TCCKS byte 1,
        // bGuardTimeT0 at byte 2 (255 = the ISO reduced-guard-time
        // sentinel, which the bench ComSign eID's TC1 actually carries).
        let t0_params: [u8; 5] = [0x11, 0x00, 0xFF, 0x0A, 0x00];
        core.process_command(
            &build(PC_TO_RDR_SET_PARAMETERS, 4, [0, 0, 0], &t0_params),
            &mut resp,
        );
        assert_eq!(
            core.accepted_contact_timing(),
            Some(ContactWireTiming {
                fi: 372,
                di: 1,
                guard_time_n: 0xFF,
                protocol: 0,
            })
        );
    }

    #[test]
    fn set_parameters_rejects_bad_fields_with_error_08_parameters_shape() {
        let mut core = new_core(true, &T1_ATR_BWI3_CWI7, false);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        // bIFSC = 0x05 (below 0x10)
        let len = core.process_command(
            &build(
                PC_TO_RDR_SET_PARAMETERS,
                3,
                [1, 0, 0],
                &[0x11, 0x00, 0x00, 0x55, 0x00, 0x05, 0x00],
            ),
            &mut resp,
        );
        let (mt, _, status, err, data) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_PARAMETERS);
        assert_eq!(status & 0xC0, COMMAND_STATUS_FAILED << 6);
        assert_eq!(err, 0x08);
        assert!(data.is_empty());
        assert!(!core.take_parameters_accepted());
    }

    /// #137 invariant, at the layer that owns the GetParameters echo:
    /// for EVERY T=1 parameter set this core can put on the wire, the
    /// shared reader first-byte policy covers the advertised BWT — or
    /// saturates at the documented 5 s ceiling (the forced
    /// no-time-extension decision; see
    /// `ccid_protocol::atr::T1_FIRST_BYTE_MAX_MS`). Both BWI sources
    /// are unchecked nibbles 0..=15: the ATR-derived echo and the
    /// host-SetParameters verbatim echo.
    #[test]
    fn get_parameters_echo_bwi_always_covered_by_first_byte_policy() {
        use ccid_protocol::atr::{t1_bwt_ms, t1_first_byte_timeout_ms};

        const BAUDS: [u32; 4] = [9_600, 10_752, 12_903, 125_000];

        fn check(bwi: u8) {
            for baud in BAUDS {
                let bwt = t1_bwt_ms(bwi, baud);
                let fb = t1_first_byte_timeout_ms(bwi, baud);
                if bwt <= ccid_protocol::atr::T1_FIRST_BYTE_MAX_MS {
                    assert!(
                        fb >= bwt,
                        "bwi={bwi} baud={baud}: first-byte {fb} < advertised BWT {bwt}"
                    );
                } else {
                    assert_eq!(
                        fb,
                        ccid_protocol::atr::T1_FIRST_BYTE_MAX_MS,
                        "bwi={bwi} baud={baud}: above-cap BWT must saturate"
                    );
                }
            }
        }

        let mut core = new_core(true, &T1_ATR_BWI3_CWI7, false);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];

        // ATR-derived echo (TB3 = BWI 3 / CWI 7 → 0x37)
        let len = core.process_command(&build(PC_TO_RDR_GET_PARAMETERS, 2, [0; 3], &[]), &mut resp);
        let (_, _, _, _, data) = parse(&resp[..len]);
        assert_eq!(data.len(), 7);
        assert_eq!(data[3], 0x37);
        check(data[3] >> 4);

        // Host-supplied echo: every BWI nibble is accepted and echoed
        // verbatim — the reader must stay as patient as each one it
        // advertises (or saturate knowingly).
        for bwi in 0u8..=15 {
            let params: [u8; 7] = [0x11, 0x00, 0x00, (bwi << 4) | 5, 0x00, 0xFE, 0x00];
            let len = core.process_command(
                &build(PC_TO_RDR_SET_PARAMETERS, 3, [1, 0, 0], &params),
                &mut resp,
            );
            let (_, _, status, _, data) = parse(&resp[..len]);
            assert_eq!(status & 0xC0, 0, "bwi={bwi} accepted");
            assert_eq!(data, params, "bwi={bwi} echoed verbatim");
            check(data[3] >> 4);
        }
    }

    #[test]
    fn set_parameters_invalid_length_answers_slot_status_07() {
        let mut core = new_core(true, &T0_ATR, false);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let len = core.process_command(
            &build(PC_TO_RDR_SET_PARAMETERS, 2, [0, 0, 0], &[0x11, 0x00, 0x00]),
            &mut resp,
        );
        let (mt, _, _, err, _) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(err, 0x07);
    }

    #[test]
    fn reset_parameters_restores_t0_defaults() {
        let mut core = new_core(true, &T1_ATR_BWI3_CWI7, false);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        core.process_command(
            &build(
                PC_TO_RDR_SET_PARAMETERS,
                2,
                [1, 0, 0],
                &[0x11, 0x00, 0x00, 0x55, 0x00, 0xFE, 0x00],
            ),
            &mut resp,
        );
        let len = core.process_command(
            &build(PC_TO_RDR_RESET_PARAMETERS, 3, [0; 3], &[]),
            &mut resp,
        );
        let (mt, _, _, _, data) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_PARAMETERS);
        assert_eq!(resp[9], 0);
        assert_eq!(data, [0x11, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(core.current_protocol(), 0);
    }

    #[test]
    fn xfr_block_inactive_answers_slot_status_0xfe() {
        let mut core = new_core(true, &T0_ATR, false);
        let mut resp = [0u8; 271];
        let len = core.process_command(
            &build(PC_TO_RDR_XFR_BLOCK, 1, [0; 3], &[0x00, 0xA4, 0x04, 0x00]),
            &mut resp,
        );
        let (mt, _, status, err, data) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(status & 0xC0, COMMAND_STATUS_FAILED << 6);
        assert_eq!(err, 0xFE);
        assert!(data.is_empty());
    }

    #[test]
    fn xfr_block_relay_failure_answers_datablock_timeout_sw_and_stays_active() {
        let mut core = new_core(true, &T0_ATR, false);
        power_on(&mut core, 1);
        core.driver_mut().fail_transmit = true;
        let mut resp = [0u8; 271];
        let len = core.process_command(
            &build(PC_TO_RDR_XFR_BLOCK, 2, [0; 3], &[0x00, 0xA4, 0x04, 0x00]),
            &mut resp,
        );
        let (mt, _, status, err, data) = parse(&resp[..len]);
        // DataBlock (never SlotStatus — the jnasmartcardio 0-byte
        // SW-strip crash, gp #307), FAILED + 0xF0: libccid synthesizes
        // SW 64 00 host-side — the clean card-timeout shape. The mock's
        // default class is Unclassified → the fold keeps this shape
        // (#136 behavior-preserving default).
        assert_eq!(mt, RDR_TO_PC_DATABLOCK);
        assert_eq!(
            status,
            COMMAND_STATUS_FAILED << 6 | ICC_STATUS_PRESENT_ACTIVE
        );
        assert_eq!(err, CCID_ERR_CARD_TIMEOUT);
        assert!(data.is_empty());
        assert_eq!(core.slot_state(), SlotState::PresentActive);
    }

    #[test]
    fn xfr_block_parity_failure_answers_datablock_berror_0xfd() {
        // #136: a classified parity failure must surface as CCID
        // bError 0xFD — libccid's CCID_Receive `case 0xFD` turns this
        // into IFD_PARITY_ERROR so the HOST T=0/T=1 engine owns the
        // retry. Folding parity into 0xF0 would synthesize a fake
        // SW 64 00 "card answered negatively" for a wire corruption
        // the card never sent.
        let mut core = new_core(true, &T0_ATR, false);
        power_on(&mut core, 1);
        core.driver_mut().fail_transmit = true;
        core.driver_mut().transmit_class = XfrErrorKind::Parity;
        let mut resp = [0u8; 271];
        let len = core.process_command(
            &build(PC_TO_RDR_XFR_BLOCK, 2, [0; 3], &[0x00, 0xA4, 0x04, 0x00]),
            &mut resp,
        );
        let (mt, _, status, err, data) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_DATABLOCK);
        assert_eq!(
            status,
            COMMAND_STATUS_FAILED << 6 | ICC_STATUS_PRESENT_ACTIVE
        );
        assert_eq!(err, CCID_ERR_XFR_PARITY_ERROR);
        assert_eq!(err, 0xFD);
        assert!(data.is_empty());
    }

    #[test]
    fn xfr_block_mute_failure_answers_datablock_berror_0xfe() {
        // The osmo ccid_slot_fsm.c WTIME_EXP shape: FAILED DataBlock
        // with ICC_MUTE (see the error-map table's OSMO_FSM quote).
        let mut core = new_core(true, &T0_ATR, false);
        power_on(&mut core, 1);
        core.driver_mut().fail_transmit = true;
        core.driver_mut().transmit_class = XfrErrorKind::Mute;
        let mut resp = [0u8; 271];
        let len = core.process_command(
            &build(PC_TO_RDR_XFR_BLOCK, 2, [0; 3], &[0x00, 0xA4, 0x04, 0x00]),
            &mut resp,
        );
        let (mt, _, _, err, _) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_DATABLOCK);
        assert_eq!(err, CCID_ERR_ICC_MUTE);
    }

    #[test]
    fn xfr_error_map_table_pins_every_entry() {
        // The #136 error-map table, byte-pinned. Values are CCID Rev
        // 1.1 Table 6.2-2 codes; the host consequences per entry are
        // documented on `xfr_failure_b_error` (libccid CCID_Receive
        // switch quotes).
        use XfrErrorKind::*;
        assert_eq!(xfr_failure_b_error(Parity), CCID_ERR_XFR_PARITY_ERROR);
        assert_eq!(xfr_failure_b_error(Parity), 0xFD);
        assert_eq!(xfr_failure_b_error(Overrun), CCID_ERR_XFR_OVERRUN);
        assert_eq!(xfr_failure_b_error(Overrun), 0xFC);
        assert_eq!(xfr_failure_b_error(Timeout), CCID_ERR_CARD_TIMEOUT);
        assert_eq!(xfr_failure_b_error(Timeout), 0xF0);
        assert_eq!(xfr_failure_b_error(Mute), CCID_ERR_ICC_MUTE);
        assert_eq!(xfr_failure_b_error(Mute), 0xFE);
        assert_eq!(xfr_failure_b_error(Hardware), CCID_ERR_HW_ERROR);
        assert_eq!(xfr_failure_b_error(Hardware), 0xFB);
        assert_eq!(xfr_failure_b_error(Unclassified), CCID_ERR_CARD_TIMEOUT);
        assert_eq!(xfr_failure_b_error(Unclassified), 0xF0);
    }

    #[test]
    fn xfr_block_pps_echoed_and_forwarded_apdu_answered() {
        let mut core = new_core(true, &T0_ATR, false);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let pps = [0xFF, 0x11, 0x18, 0xF6];
        let len = core.process_command(&build(PC_TO_RDR_XFR_BLOCK, 2, [0; 3], &pps), &mut resp);
        let (mt, _, status, _, data) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_DATABLOCK);
        assert_eq!(status & 0xC0, 0);
        assert_eq!(data, pps);

        // CLA=0xFF pseudo-APDUs are NOT intercepted on contact readers:
        // FF CA 00 00 (GET UID) goes to the card.
        let mut resp = [0u8; 271];
        let len = core.process_command(
            &build(PC_TO_RDR_XFR_BLOCK, 3, [0; 3], &[0xFF, 0xCA, 0x00, 0x00]),
            &mut resp,
        );
        let (_, _, _, _, data) = parse(&resp[..len]);
        assert_eq!(data, [0x90, 0x00]);
    }

    #[test]
    fn abort_answers_success_on_contact_profile() {
        let mut core = new_core(false, &T0_ATR, false);
        let mut resp = [0u8; 271];
        let len = core.process_command(&build(PC_TO_RDR_ABORT, 0x7A, [0; 3], &[]), &mut resp);
        let (mt, seq, status, err, _) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_SLOTSTATUS);
        assert_eq!(seq, 0x7A);
        assert_eq!(status & 0xC0, 0);
        assert_eq!(err, 0);
    }

    #[test]
    fn card_removal_powers_off_driver_and_cancels_secure() {
        let mut core = new_core(true, &T0_ATR, true);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let mut secure_payload = [0x00u8].to_vec();
        secure_payload.extend_from_slice(&pin_verify_probe());
        let secure = build(PC_TO_RDR_SECURE, 0x55, [0x00, 0, 0], &secure_payload);
        let len = core.process_command(&secure, &mut resp);
        assert_eq!(len, 0, "PIN entry is asynchronous");
        assert!(core.is_pin_verify_active());

        core.driver_mut().present = false;
        assert!(core.check_card_change().is_some());
        assert_eq!(core.slot_state(), SlotState::Absent);
        assert_eq!(core.driver_mut().power_off_count, 1);
        assert!(!core.is_pin_verify_active());
    }

    #[test]
    fn secure_non_pin_pad_answers_data_block_not_supported() {
        let mut core = new_core(true, &T0_ATR, false);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let mut secure_payload = [0x00u8].to_vec();
        secure_payload.extend_from_slice(&pin_verify_probe());
        let secure = build(PC_TO_RDR_SECURE, 0x55, [0x00, 0, 0], &secure_payload);
        let len = core.process_command(&secure, &mut resp);
        let (mt, _, status, err, _) = parse(&resp[..len]);
        assert_eq!(mt, RDR_TO_PC_DATABLOCK);
        assert_eq!(status & 0xC0, COMMAND_STATUS_FAILED << 6);
        assert_eq!(err, CCID_ERR_CMD_NOT_SUPPORTED);
        assert!(!core.is_pin_verify_active());
    }

    #[test]
    fn secure_verify_arms_and_completes_with_card_response() {
        let mut core = new_core(true, &T0_ATR, true);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let mut secure_payload = [0x00u8].to_vec();
        secure_payload.extend_from_slice(&pin_verify_probe());
        let secure = build(PC_TO_RDR_SECURE, 0x55, [0x00, 0, 0], &secure_payload);
        let len = core.process_command(&secure, &mut resp);
        assert_eq!(len, 0);
        assert!(core.is_pin_verify_active());

        let (seq, params) = core.take_secure_params().expect("armed");
        assert_eq!(seq, 0x55);
        assert_eq!(params.min_len, 4);
        assert!(!core.is_pin_verify_active(), "take consumes the session");

        let mut resp = [0u8; 271];
        let n = core.complete_pin_entry(seq, PinResult::Success, Some(&[0x90, 0x00]), &mut resp);
        let (mt, s, status, err, data) = parse(&resp[..n]);
        assert_eq!(mt, RDR_TO_PC_DATABLOCK);
        assert_eq!(s, 0x55);
        assert_eq!(status & 0xC0, 0);
        assert_eq!(err, 0);
        assert_eq!(data, [0x90, 0x00]);
    }

    #[test]
    fn secure_cancelled_and_timeout_error_codes() {
        let mut core = new_core(true, &T0_ATR, true);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let n = core.complete_pin_entry(9, PinResult::Cancelled, None, &mut resp);
        let (_, _, status, err, _) = parse(&resp[..n]);
        assert_eq!(status & 0xC0, COMMAND_STATUS_FAILED << 6);
        assert_eq!(err, CCID_ERR_PIN_CANCELLED);

        let n = core.complete_pin_entry(9, PinResult::Timeout, None, &mut resp);
        let (_, _, _, err, _) = parse(&resp[..n]);
        assert_eq!(err, CCID_ERR_PIN_TIMEOUT);

        let n = core.complete_pin_entry(9, PinResult::InvalidLength, None, &mut resp);
        let (_, _, _, err, _) = parse(&resp[..n]);
        assert_eq!(err, CCID_ERR_CMD_ABORTED);
    }

    /// #276: `transmit_counted` is the seam wrapper-driven card exchanges
    /// (the STM32 PIN-pad flow) must use — tx on every attempt, rx only
    /// on a card answer, identical to the XfrBlock counting.
    #[test]
    fn transmit_counted_advances_apdu_counters() {
        let mut core = new_core(true, &T0_ATR, false);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];

        assert_eq!(core.diagnostics().apdu_tx_count, 0);
        assert_eq!(core.diagnostics().apdu_rx_count, 0);

        let n = core
            .transmit_counted(&[0x00, 0x20, 0x00, 0x82, 0x06], &mut resp)
            .expect("card answers");
        assert_eq!(&resp[..n], &[0x90, 0x00]);
        assert_eq!(core.diagnostics().apdu_tx_count, 1);
        assert_eq!(core.diagnostics().apdu_rx_count, 1);

        core.driver_mut().fail_transmit = true;
        assert!(core
            .transmit_counted(&[0x00, 0x20, 0x00, 0x82, 0x06], &mut resp)
            .is_err());
        assert_eq!(core.diagnostics().apdu_tx_count, 2);
        assert_eq!(
            core.diagnostics().apdu_rx_count,
            1,
            "a failed transmit counts tx but never rx"
        );
    }

    #[test]
    fn secure_modify_arms_with_params() {
        let mut core = new_core(true, &T0_ATR, true);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let mut secure_payload = [0x01u8].to_vec();
        secure_payload.extend_from_slice(&pin_modify_probe());
        let secure = build(PC_TO_RDR_SECURE, 0x66, [0x00, 0, 0], &secure_payload);
        let len = core.process_command(&secure, &mut resp);
        assert_eq!(len, 0);
        assert!(core.is_pin_modify_active());
        let (seq, params) = core.take_secure_modify_params().expect("armed");
        assert_eq!(seq, 0x66);
        assert_eq!(params.min_len, 4);
    }

    /// §6.1.11 probe in the pcscd wire shape (reference-locked in
    /// `secure.rs` tests): wPINMaxExtraDigit wire bytes `[max, min]`,
    /// bTeoPrologue[3], APDU head at offset 14.
    fn pin_verify_probe() -> [u8; 18] {
        [
            30, 0x00, 0x00, 0x00, 8, 4, 0x02, 1, 0x09, 0x04, 0, 0, 0, 0, 0x00, 0x20, 0x00, 0x81,
        ]
    }

    /// §6.1.12 probe in the pcscd wire shape (full form, bNumberMessage=3):
    /// wPINMaxExtraDigit `[8, 4]`, bConfirmPIN=3, msg1/2/3, prologue[3],
    /// APDU head `00 24 00 81` at offset 19.
    fn pin_modify_probe() -> [u8; 23] {
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
        d
    }

    // ---- PIN-result exchanges (track I, issue #121): the §6.1.11/12
    // ---- tails moved from the STM32 wrapper. These lock the APDU the
    // ---- card actually receives, the DataBlock answer and the #276
    // ---- counter accounting — the firmware `secure_counter_tests`
    // ---- run the same contracts through the wrapper delegation.

    fn pin_buffer(digits: &[u8]) -> PinBuffer {
        let mut b = PinBuffer::new(8);
        for &d in digits {
            assert!(b.push(d));
        }
        b
    }

    fn armed_verify() -> (Core, u8, PinVerifyParams) {
        let mut core = new_core(true, &T0_ATR, true);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let mut payload = [0x00u8].to_vec();
        payload.extend_from_slice(&pin_verify_probe());
        let len = core.process_command(
            &build(PC_TO_RDR_SECURE, 0x55, [0, 0, 0], &payload),
            &mut resp,
        );
        assert_eq!(len, 0, "Secure verify defers its response");
        let (seq, params) = core.take_secure_params().expect("armed");
        (core, seq, params)
    }

    #[test]
    fn pin_verify_exchange_sends_template_apdu_and_answers_card_sw() {
        let (mut core, seq, params) = armed_verify();
        let mut resp = [0u8; 271];

        let n = core.complete_pin_verify_exchange(
            seq,
            PinResult::Success,
            pin_buffer(&[1, 2, 3, 4, 5, 6]),
            &params,
            &mut resp,
        );

        // The card saw the reader-built VERIFY APDU: template head
        // `00 20 00 81` + Lc 6 + ASCII "123456".
        assert_eq!(
            core.driver().last_apdu,
            [0x00, 0x20, 0x00, 0x81, 0x06, b'1', b'2', b'3', b'4', b'5', b'6']
        );
        // DataBlock carries the card SW, counters advance (#276).
        let (mt, s, status, err, data) = parse(&resp[..n]);
        assert_eq!(mt, RDR_TO_PC_DATABLOCK);
        assert_eq!(s, 0x55);
        assert_eq!(status & 0xC0, 0);
        assert_eq!(err, 0);
        assert_eq!(&data[..2], &[0x90, 0x00]);
        assert_eq!(core.diagnostics().apdu_tx_count, 1);
        assert_eq!(core.diagnostics().apdu_rx_count, 1);
    }

    #[test]
    fn pin_verify_exchange_transmit_failure_answers_cancelled() {
        let (mut core, seq, params) = armed_verify();
        core.driver_mut().fail_transmit = true;
        let mut resp = [0u8; 271];

        let n = core.complete_pin_verify_exchange(
            seq,
            PinResult::Success,
            pin_buffer(&[1, 2, 3, 4, 5, 6]),
            &params,
            &mut resp,
        );

        let (_, _, status, err, data) = parse(&resp[..n]);
        assert_eq!(status & 0xC0, COMMAND_STATUS_FAILED << 6);
        assert_eq!(err, CCID_ERR_PIN_CANCELLED);
        assert!(data.is_empty());
        // The attempt reached the card seam: tx counted, rx not.
        assert_eq!(core.diagnostics().apdu_tx_count, 1);
        assert_eq!(core.diagnostics().apdu_rx_count, 0);
    }

    #[test]
    fn pin_verify_exchange_build_failure_answers_invalid_length() {
        let (mut core, seq, params) = armed_verify();
        let mut resp = [0u8; 271];

        // 3 digits: below the builder's user-PIN floor (6) — the APDU
        // build rejects and the answer maps to InvalidLength (0xEB).
        let n = core.complete_pin_verify_exchange(
            seq,
            PinResult::Success,
            pin_buffer(&[1, 2, 3]),
            &params,
            &mut resp,
        );

        let (_, _, status, err, _) = parse(&resp[..n]);
        assert_eq!(status & 0xC0, COMMAND_STATUS_FAILED << 6);
        assert_eq!(err, CCID_ERR_CMD_ABORTED);
        assert_eq!(core.diagnostics().apdu_tx_count, 0, "no card transmit");
    }

    #[test]
    fn pin_verify_exchange_non_success_results_answer_directly() {
        let (mut core, seq, params) = armed_verify();
        let mut resp = [0u8; 271];

        let n = core.complete_pin_verify_exchange(
            seq,
            PinResult::Cancelled,
            pin_buffer(&[]),
            &params,
            &mut resp,
        );
        let (_, _, status, err, _) = parse(&resp[..n]);
        assert_eq!(status & 0xC0, COMMAND_STATUS_FAILED << 6);
        assert_eq!(err, CCID_ERR_PIN_CANCELLED);
        assert_eq!(core.diagnostics().apdu_tx_count, 0);
    }

    #[test]
    fn pin_modify_exchange_sends_change_apdu_and_answers_card_sw() {
        let mut core = new_core(true, &T0_ATR, true);
        power_on(&mut core, 1);
        let mut resp = [0u8; 271];
        let mut payload = [0x01u8].to_vec();
        payload.extend_from_slice(&pin_modify_probe());
        let len = core.process_command(
            &build(PC_TO_RDR_SECURE, 0x66, [0, 0, 0], &payload),
            &mut resp,
        );
        assert_eq!(len, 0);
        let (seq, params) = core.take_secure_modify_params().expect("armed");

        let n = core.complete_pin_modify_exchange(
            seq,
            PinResult::Success,
            pin_buffer(&[9, 9, 9, 9, 9, 9]),
            pin_buffer(&[1, 2, 3, 4, 5, 6]),
            &params,
            &mut resp,
        );

        // CHANGE REFERENCE DATA: head `00 24 00 81`, Lc 12, old then new.
        assert_eq!(
            core.driver().last_apdu,
            [
                0x00, 0x24, 0x00, 0x81, 0x0C, b'9', b'9', b'9', b'9', b'9', b'9', b'1', b'2', b'3',
                b'4', b'5', b'6'
            ]
        );
        let (mt, s, status, err, data) = parse(&resp[..n]);
        assert_eq!(mt, RDR_TO_PC_DATABLOCK);
        assert_eq!(s, 0x66);
        assert_eq!(status & 0xC0, 0);
        assert_eq!(err, 0);
        assert_eq!(&data[..2], &[0x90, 0x00]);
        assert_eq!(core.diagnostics().apdu_tx_count, 1);
        assert_eq!(core.diagnostics().apdu_rx_count, 1);
    }

    // ---- Escape 0xD0 + the profile-gated serial escape table
    // ---- (track I, #121): the delegated contact escape surface.

    #[test]
    fn escape_d0_answers_live_counters_with_driver_fold() {
        // The cache starts ABSENT (constructed with no card); the
        // driver's live view says a card IS in — the fold must back
        // card_present in the answer while keeping core counters.
        let mut core: Core =
            CcidHandlerCore::with_profile(ContactDriver::new(false, &T0_ATR), b"core", false)
                .with_diagnostics_fold(|drv, diag| {
                    if let Some(p) = drv.diag_card_present {
                        diag.card_present = p;
                    }
                });
        core.driver_mut().present = true; // card lands between polls
        core.driver_mut().diag_card_present = Some(true);

        let mut resp = [0u8; 271];
        let n = core.process_command(
            &build(PC_TO_RDR_ESCAPE, 0x42, [0, 0, 0], &[0xD0]),
            &mut resp,
        );
        let (mt, seq, status, err, data) = parse(&resp[..n]);
        assert_eq!(mt, RDR_TO_PC_ESCAPE);
        assert_eq!(seq, 0x42);
        assert_eq!(status & 0xC0, 0);
        assert_eq!(err, 0);
        let diag = Diagnostics::from_bytes(&data[..Diagnostics::SERIALIZED_SIZE]).expect("decode");
        assert!(diag.card_present, "the fold backs the driver's live view");
        assert_eq!(diag.apdu_tx_count, 0, "core counters survive the fold");
    }

    #[test]
    fn escape_d0_without_fold_answers_the_cached_snapshot() {
        // Default (no fold installed): byte-identical to the pre-fold
        // core answer — the ESP32 shape.
        let mut core = new_core(false, &T0_ATR, false);
        let mut resp = [0u8; 271];
        let n = core.process_command(
            &build(PC_TO_RDR_ESCAPE, 0x43, [0, 0, 0], &[0xD0]),
            &mut resp,
        );
        let (mt, _, status, err, data) = parse(&resp[..n]);
        assert_eq!(mt, RDR_TO_PC_ESCAPE);
        assert_eq!(status & 0xC0, 0);
        assert_eq!(err, 0);
        let diag = Diagnostics::from_bytes(&data[..Diagnostics::SERIALIZED_SIZE]).expect("decode");
        assert!(!diag.card_present, "no fold: the polled cache answers");
    }

    #[test]
    fn escape_serial_table_rejected_on_contact_profile() {
        // 0x02 (identity), 0xD1 (snapshot), 0x01-01-01 (sync) are the
        // GemPC-Twin SERIAL surface — the contact wrapper rejected them
        // pre-port, so the delegated contact table must too.
        let mut core = new_core(false, &T0_ATR, false);
        let mut resp = [0u8; 271];
        for (seq, payload) in [
            (0x51u8, &[0x02u8][..]),
            (0x52, &[0xD1][..]),
            (0x53, &[0x01, 0x01, 0x01][..]),
        ] {
            let n =
                core.process_command(&build(PC_TO_RDR_ESCAPE, seq, [0, 0, 0], payload), &mut resp);
            let (mt, _, status, err, _) = parse(&resp[..n]);
            assert_eq!(mt, RDR_TO_PC_ESCAPE, "seq {seq}");
            assert_eq!(status & 0xC0, COMMAND_STATUS_FAILED << 6, "seq {seq}");
            assert_eq!(err, CCID_ERR_CMD_NOT_SUPPORTED, "seq {seq}");
        }
        assert!(!core.take_snapshot_request(), "0xD1 never arms on contact");
        assert!(!core.sync_notifications(), "sync never arms on contact");
    }
}
