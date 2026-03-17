#![cfg(all(target_arch = "arm", target_os = "none"))]
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_is_multiple_of)]
//! USB CCID Class implementation for smartcard reader mode
//!
//! This module implements the USB Chip/Smart Card Interface Device (CCID) protocol
//! as defined in the CCID Specification Rev 1.1 for smartcard reader functionality.

use usb_device::class_prelude::*;
use usb_device::endpoint::{In, Out};
use usb_device::{Result, UsbError};

use crate::pinpad::{
    ModifyApduBuilder, PinBuffer, PinModifyParams, PinResult, PinVerifyParams, VerifyApduBuilder,
};
use crate::smartcard::{parse_atr, AtrParams};

// ============================================================================
// CCID Message Types (Bulk OUT - Host to Device)
// ============================================================================

/// PC_to_RDR_IccPowerOn - Apply power to ICC and get ATR
pub const PC_TO_RDR_ICC_POWER_ON: u8 = 0x62;
/// PC_to_RDR_IccPowerOff - Remove power from ICC
pub const PC_TO_RDR_ICC_POWER_OFF: u8 = 0x63;
/// PC_to_RDR_GetSlotStatus - Get slot status
pub const PC_TO_RDR_GET_SLOT_STATUS: u8 = 0x65;
/// PC_to_RDR_XfrBlock - Transfer data block (APDU)
pub const PC_TO_RDR_XFR_BLOCK: u8 = 0x6F;
/// PC_to_RDR_GetParameters - Get protocol parameters
pub const PC_TO_RDR_GET_PARAMETERS: u8 = 0x6C;
/// PC_to_RDR_SetParameters - Set protocol parameters
pub const PC_TO_RDR_SET_PARAMETERS: u8 = 0x61;
/// PC_to_RDR_Secure - PIN verification (not supported)
pub const PC_TO_RDR_SECURE: u8 = 0x69;
/// PC_to_RDR_T0APDU - T=0 APDU (not supported, use XfrBlock)
pub const PC_TO_RDR_T0_APDU: u8 = 0x6A;
/// PC_to_RDR_Escape - Vendor-specific (not supported)
pub const PC_TO_RDR_ESCAPE: u8 = 0x6B;
/// PC_to_RDR_ResetParameters - Reset params (not supported)
pub const PC_TO_RDR_RESET_PARAMETERS: u8 = 0x6D;
/// PC_to_RDR_IccClock - Clock control (not supported)
pub const PC_TO_RDR_ICC_CLOCK: u8 = 0x6E;
/// PC_to_RDR_Mechanical - Mechanical (not supported)
pub const PC_TO_RDR_MECHANICAL: u8 = 0x71;
/// PC_to_RDR_Abort - Abort current command
pub const PC_TO_RDR_ABORT: u8 = 0x72;
/// PC_to_RDR_SetDataRateAndClockFrequency - Set rate/clock (not supported)
pub const PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ: u8 = 0x73;

// ============================================================================
// CCID Message Types (Bulk IN - Device to Host)
// ============================================================================

/// RDR_to_PC_DataBlock - Response with data (ATR, APDU response)
pub const RDR_TO_PC_DATABLOCK: u8 = 0x80;
/// RDR_to_PC_SlotStatus - Response with slot status
pub const RDR_TO_PC_SLOTSTATUS: u8 = 0x81;
/// RDR_to_PC_Parameters - Response with protocol parameters
pub const RDR_TO_PC_PARAMETERS: u8 = 0x82;
/// RDR_to_PC_Escape - Response to Escape command
pub const RDR_TO_PC_ESCAPE: u8 = 0x83;
/// RDR_to_PC_DataRateAndClockFrequency - Response with data rate/clock
pub const RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ: u8 = 0x84;
/// RDR_to_PC_NotifySlotChange - Interrupt message for slot change (sent on ep_int)
pub const RDR_TO_PC_NOTIFY_SLOT_CHANGE: u8 = 0x50;

// ============================================================================
// CCID Class-Specific Requests
// ============================================================================

/// Abort command
pub const REQUEST_ABORT: u8 = 0x01;
/// Get supported clock frequencies
pub const REQUEST_GET_CLOCK_FREQUENCIES: u8 = 0x02;
/// Get supported data rates
pub const REQUEST_GET_DATA_RATES: u8 = 0x03;

// ============================================================================
// CCID Descriptor Constants
// ============================================================================

/// CCID interface class code
pub const CLASS_CCID: u8 = 0x0B;
/// CCID subclass (none)
pub const SUBCLASS_NONE: u8 = 0x00;
/// CCID protocol (bulk transfer)
pub const PROTOCOL_BULK: u8 = 0x00;
/// CCID functional descriptor type
pub const DESCRIPTOR_TYPE_CCID: u8 = 0x21;

/// Maximum packet size for full-speed USB
pub const PACKET_SIZE: usize = 64;
/// CCID header size (10 bytes)
pub const CCID_HEADER_SIZE: usize = 10;
/// Maximum CCID message length (header + 261 data bytes)
pub const MAX_CCID_MESSAGE_LENGTH: usize = 271;

// ============================================================================
// Slot Status Constants (bmICCStatus)
// ============================================================================

/// ICC present and active
pub const ICC_STATUS_PRESENT_ACTIVE: u8 = 0x00;
/// ICC present but inactive (not powered)
pub const ICC_STATUS_PRESENT_INACTIVE: u8 = 0x01;
/// No ICC present
pub const ICC_STATUS_NO_ICC: u8 = 0x02;

// ============================================================================
// Command Status Constants (bmCommandStatus)
// ============================================================================

/// Command processed successfully
pub const COMMAND_STATUS_NO_ERROR: u8 = 0x00;
/// Command failed
pub const COMMAND_STATUS_FAILED: u8 = 0x01;
/// Command time extension requested
pub const COMMAND_STATUS_TIME_EXTENSION: u8 = 0x02;

/// CCID error codes (bError field) — CCID Spec Table 6.2-2 / osmo ccid_proto.h
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

// ===============================================================================
// CCID Class Functional Descriptor
//
// Reference: CCID Rev 1.1 Spec (USB-IF DWG_Smart-Card_CCID_Rev110.pdf)
// Table 5.1-1: CCID Functional Descriptor Fields
//
// The descriptor is generated from the active device profile.
// See src/device_profile.rs for profile configuration.
use crate::device_profile::CURRENT_PROFILE;

/// CCID class descriptor data from the active device profile
pub const CCID_CLASS_DESCRIPTOR_DATA: [u8; 52] = CURRENT_PROFILE.ccid_descriptor();
/// Default clock frequency in kHz for class requests
pub const CLOCK_FREQUENCY_KHZ: [u8; 4] = [0x40, 0x0F, 0x00, 0x00]; // 4000 kHz = 4 MHz

/// Default data rate for class requests
pub const DATA_RATE_BPS: [u8; 4] = [0x00, 0x2A, 0x00, 0x00]; // 10752 bps

// ============================================================================
// SmartcardDriver Trait
// ============================================================================

/// Trait for smartcard driver implementations
///
/// This trait defines the interface between the CCID class and the underlying
/// smartcard hardware driver. Implement this trait to provide smartcard functionality.
pub trait SmartcardDriver {
    /// Error type for driver operations
    type Error: core::fmt::Debug;

    /// Power on the smartcard and return the Answer-to-Reset (ATR)
    ///
    /// Returns the ATR bytes on success. The ATR typically ranges from 2 to 33 bytes.
    fn power_on(&mut self) -> core::result::Result<&[u8], Self::Error>;

    /// Power off the smartcard
    fn power_off(&mut self);

    /// Check if a smartcard is present in the slot
    fn is_card_present(&self) -> bool;

    /// Transmit an APDU command to the smartcard
    ///
    /// # Arguments
    /// * `command` - The APDU command bytes to send
    /// * `response` - Buffer to store the response
    ///
    /// # Returns
    /// The number of bytes written to the response buffer
    fn transmit_apdu(
        &mut self,
        command: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error>;

    /// Transmit raw bytes to the smartcard and receive raw response.
    /// Used for TPDU mode where the host sends complete T=1 blocks.
    ///
    /// # Arguments
    /// * `data` - Raw bytes to transmit (complete T=1 block)
    /// * `response` - Buffer to store the complete response T=1 block
    ///
    /// # Returns
    /// The number of bytes written to the response buffer (NAD+PCB+LEN+INF+LRC)
    fn transmit_raw(
        &mut self,
        data: &[u8],
        response: &mut [u8],
    ) -> core::result::Result<usize, Self::Error>;

    /// Set the communication protocol (0 = T=0, 1 = T=1)
    fn set_protocol(&mut self, protocol: u8);

    /// Set ICC clock output (IccClock command). `enable`: true = restart clock, false = stop clock.
    fn set_clock(&mut self, _enable: bool) {
        // Default: no-op (e.g. hardware does not support clock stop)
    }

    /// Set data rate and clock frequency (SetDataRateAndClockFrequency command).
    /// Returns (actual_clock_hz, actual_rate_bps) on success.
    fn set_clock_and_rate(
        &mut self,
        _clock_hz: u32,
        _rate_bps: u32,
    ) -> core::result::Result<(u32, u32), Self::Error>;
}

// ============================================================================
// CCID Message Structures
// ============================================================================

/// Slot state for 3-state FSM
#[derive(Clone, Copy, PartialEq)]
enum SlotState {
    Absent,          // No card in slot
    PresentInactive, // Card present but not powered
    PresentActive,   // Card present, powered, ATR received
}

/// Secure PIN entry state for PC_to_RDR_Secure operations
#[derive(Clone)]
pub enum SecureState {
    /// No active PIN entry
    Idle,
    /// PIN verify entry in progress - waiting for display/touch input
    WaitingForPinVerify {
        /// CCID sequence number for response
        seq: u8,
        /// Parsed PIN verify parameters
        params: PinVerifyParams,
    },
    /// PIN modify entry in progress - waiting for display/touch input
    WaitingForPinModify {
        /// CCID sequence number for response
        seq: u8,
        /// Parsed PIN modify parameters
        params: PinModifyParams,
    },
}

/// USB CCID Class for smartcard reader functionality
pub struct CcidClass<'bus, Bus: UsbBus, D: SmartcardDriver> {
    /// Interface number
    interface: InterfaceNumber,
    /// Bulk IN endpoint (device to host)
    ep_in: EndpointIn<'bus, Bus>,
    /// Bulk OUT endpoint (host to device)
    ep_out: EndpointOut<'bus, Bus>,
    /// Smartcard driver
    driver: D,
    /// Receive buffer
    rx_buffer: [u8; MAX_CCID_MESSAGE_LENGTH],
    /// Receive buffer position
    rx_len: usize,
    /// Transmit buffer
    tx_buffer: [u8; MAX_CCID_MESSAGE_LENGTH],
    /// Transmit buffer length
    tx_len: usize,
    /// Slot state (3-state FSM)
    slot_state: SlotState,
    /// Command busy flag (reject with SLOT_BUSY if true)
    cmd_busy: bool,
    /// Current offset into tx_buffer for multi-packet TX
    tx_offset: usize,
    /// True if multi-packet TX in progress
    tx_pending: bool,
    /// True if ZLP needed after last full-size packet
    needs_zlp: bool,
    /// Card presence state for edge detection
    card_present_last: bool,
    /// Interrupt IN endpoint for NotifySlotChange
    ep_int: EndpointIn<'bus, Bus>,
    /// Current protocol: 0 = T=0, 1 = T=1
    current_protocol: u8,
    /// Parsed ATR parameters (from last power-on)
    atr_params: AtrParams,
    /// Secure PIN entry state for deferred response
    secure_state: SecureState,
    /// Pending PIN result from touchscreen entry (display feature)
    /// Stores (seq, result, buffer, params) to avoid race condition with secure_state
    #[cfg(feature = "display")]
    pin_result_pending: Option<(u8, PinResult, PinBuffer, PinVerifyParams)>,
    /// Pending PIN modify result from touchscreen entry (display feature)
    /// Stores (seq, result, old_buffer, new_buffer, params) for PIN change operations
    #[cfg(feature = "display")]
    pin_modify_result_pending: Option<(u8, PinResult, PinBuffer, PinBuffer, PinModifyParams)>,
    /// Response buffer for card communication
    response_buffer: [u8; 261],
}

impl<'bus, Bus: UsbBus, D: SmartcardDriver> CcidClass<'bus, Bus, D> {
    /// Create a new CCID class instance
    ///
    /// # Arguments
    /// * `allocator` - USB bus allocator
    /// * `driver` - Smartcard driver implementation
    pub fn new(
        allocator: &'bus UsbBusAllocator<Bus>,
        driver: D,
        ep_int: EndpointIn<'bus, Bus>,
    ) -> Self {
        let mut this = Self {
            interface: allocator.interface(),
            ep_in: allocator.bulk::<In>(PACKET_SIZE as u16),
            ep_out: allocator.bulk::<Out>(PACKET_SIZE as u16),
            driver,
            rx_buffer: [0u8; MAX_CCID_MESSAGE_LENGTH],
            rx_len: 0,
            tx_buffer: [0u8; MAX_CCID_MESSAGE_LENGTH],
            tx_len: 0,
            slot_state: SlotState::Absent,
            cmd_busy: false,
            tx_offset: 0,
            tx_pending: false,
            needs_zlp: false,
            card_present_last: false,
            ep_int,
            current_protocol: 0,
            atr_params: AtrParams::default(),
            secure_state: SecureState::Idle,
            #[cfg(feature = "display")]
            pin_result_pending: None,
            #[cfg(feature = "display")]
            pin_modify_result_pending: None,
            response_buffer: [0u8; 261],
        };
        let present = this.driver.is_card_present();
        defmt::info!("CCID init: card_present={}", present);
        if present {
            this.card_present_last = true;
            this.slot_state = SlotState::PresentInactive;
        }
        this
    }

    /// Get a reference to the smartcard driver
    pub fn driver(&self) -> &D {
        &self.driver
    }

    /// Get a mutable reference to the smartcard driver
    pub fn driver_mut(&mut self) -> &mut D {
        &mut self.driver
    }

    /// Handle received CCID message
    fn handle_message(&mut self) {
        if self.rx_len < CCID_HEADER_SIZE {
            defmt::warn!("CCID: message too short");
            return;
        }

        let msg_type = self.rx_buffer[0];
        let slot = self.rx_buffer[5];
        let seq = self.rx_buffer[6];

        defmt::debug!(
            "CCID: received message type=0x{:02X}, slot={}, seq={}",
            msg_type,
            slot,
            seq
        );

        // Only support slot 0
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

            // ========================================================================
            // STUB COMMANDS - Not implemented, return CMD_NOT_SUPPORTED
            //
            // These commands are also stubs in osmo-ccid-firmware (ccid_device.c:569-630):
            // - Escape:     Returns CCID_ERR_CMD_NOT_SUPPORTED (vendor-specific)
            // - T0APDU:     Returns CCID_ERR_CMD_NOT_SUPPORTED (FIXME in osmo)
            // - Secure:     Returns CCID_ERR_CMD_NOT_SUPPORTED (FIXME in osmo, requires PIN hardware)
            // - Mechanical: Returns CCID_ERR_CMD_NOT_SUPPORTED (no mechanical parts)
            //
            // Full implementation is not needed for this reader:
            // - Escape:     Vendor-specific extended commands (reader-dependent)
            // - T0APDU:     T=0 APDU level control (TPDU level is sufficient)
            // - Secure:     PIN entry/verification (requires keypad hardware we don't have)
            // - Mechanical: Card eject/capture (no mechanical parts in this reader)
            // ========================================================================
            PC_TO_RDR_ESCAPE => self.handle_escape(seq),
            PC_TO_RDR_ICC_CLOCK => {
                self.handle_icc_clock(seq);
            }
            PC_TO_RDR_T0_APDU => {
                defmt::debug!("CCID: T0APDU command (stub - TPDU level sufficient)");
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
            PC_TO_RDR_SECURE => {
                self.handle_secure(seq);
            }
            PC_TO_RDR_MECHANICAL => {
                defmt::debug!("CCID: Mechanical command (stub - no mechanical parts)");
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }

            // ========================================================================
            // ABORT - Minimal implementation for single-slot reader
            //
            // osmo-ccid-firmware (ccid_device.c:632-659) also has incomplete Abort:
            //   switch (0/* FIXME */) { ... }
            //   /* FIXME */
            //   resp = ccid_gen_slot_status(cs, seq, CCID_CMD_STATUS_OK, 0);
            //
            // For single-slot readers, Abort is rarely needed because:
            // 1. Commands execute sequentially (no true concurrency)
            // 2. The cmd_busy flag prevents overlapping commands
            // 3. USB bulk transfers are already atomic at the transport level
            //
            // A full implementation would require:
            // - Tracking the current command type
            // - Canceling any in-progress smartcard operation
            // - Proper state machine cleanup
            //
            // We return success (CMD_STATUS_OK) which matches osmo's behavior.
            // ========================================================================
            PC_TO_RDR_ABORT => {
                defmt::debug!("CCID: Abort command (stub - single-slot sequential execution)");
                self.send_slot_status(seq, COMMAND_STATUS_NO_ERROR, self.get_icc_status(), 0);
            }

            PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ => {
                self.handle_set_data_rate_and_clock(seq);
            }
            _ => {
                defmt::warn!("CCID: unknown message type 0x{:02X}", msg_type);
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
        }
    }

    /// Get current ICC status from slot state (per osmo-ccid 3-state FSM)
    fn get_icc_status(&self) -> u8 {
        match self.slot_state {
            SlotState::PresentActive => ICC_STATUS_PRESENT_ACTIVE,
            SlotState::PresentInactive => ICC_STATUS_PRESENT_INACTIVE,
            SlotState::Absent => ICC_STATUS_NO_ICC,
        }
    }

    /// Send NotifySlotChange on interrupt endpoint
    ///
    /// Reference: CCID Rev 1.1 §6.3.1 - RDR_to_PC_NotifySlotChange
    ///
    /// Message structure (2 bytes):
    /// - bMessageType: 0x50
    /// - bmSlotICCState: Slot state bits
    ///   - Bit 0: ICC present (0=absent, 1=present)
    ///   - Bit 1: ICC state changed (0=no change, 1=changed)
    ///   - Bits 2-7: Reserved for slot 0
    fn send_notify_slot_change(&mut self, card_present: bool, changed: bool) {
        let mut bits: u8 = 0;
        if card_present {
            bits |= 0x01;
        } // Bit 0: ICC present
        if changed {
            bits |= 0x02;
        } // Bit 1: Change occurred
        let msg = [RDR_TO_PC_NOTIFY_SLOT_CHANGE, bits]; // [0x50, bits]
        let _ = self.ep_int.write(&msg); // Best-effort, ignore errors
    }

    /// Build status byte from command status and ICC status
    fn build_status(cmd_status: u8, icc_status: u8) -> u8 {
        (cmd_status << 6) | icc_status
    }

    /// Send error response with correct type per CCID spec (per osmo gen_err_resp)
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

    /// Handle PC_to_RDR_IccPowerOn command
    ///
    /// Reference: CCID Rev 1.1 §6.1.1 - IccPowerOn
    ///
    /// Request structure (10 bytes):
    /// - bMessageType: 0x62
    /// - dwLength: 0x00000000 (no data)
    /// - bSlot: Slot number
    /// - bSeq: Sequence number
    /// - bPowerSelect: 0x00=Auto, 0x01=5V, 0x02=3V, 0x03=1.8V
    /// - abRFU: 2 bytes reserved
    ///
    /// Response: RDR_to_PC_DataBlock (0x80) per §6.2.1
    /// - dwLength: ATR length
    /// - abData: Answer-to-Reset bytes
    ///
    /// Error conditions:
    /// - ICC_MUTE (0xFE): No card present or power-on failed
    /// - CMD_NOT_SUPPORTED (0x00): Voltage not supported
    fn handle_power_on(&mut self, seq: u8) {
        // Per CCID Rev 1.1 §6.1.1: dwLength must be 0x00000000
        let data_len = u32::from_le_bytes([
            self.rx_buffer[1],
            self.rx_buffer[2],
            self.rx_buffer[3],
            self.rx_buffer[4],
        ]);
        if data_len != 0 {
            defmt::warn!("CCID: IccPowerOn with non-zero dwLength={}", data_len);
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

        // bPowerSelect at byte 7: 0x00=auto, 0x01=5V, 0x02=3V, 0x03=1.8V (CCID 6.1.1)
        let power_select = if self.rx_len > 7 {
            self.rx_buffer[7]
        } else {
            0
        };
        if power_select == 0x02 || power_select == 0x03 {
            // 3V / 1.8V not supported by this hardware
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

                // Build DataBlock response
                self.tx_buffer[0] = RDR_TO_PC_DATABLOCK;
                self.tx_buffer[1..5].copy_from_slice(&(atr_len as u32).to_le_bytes());
                self.tx_buffer[5] = 0; // slot
                self.tx_buffer[6] = seq;
                self.tx_buffer[7] =
                    Self::build_status(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE);
                self.tx_buffer[8] = 0; // bError
                self.tx_buffer[9] = 0; // bChainParameter

                // SeedKeeper is T=1 only; dwProtocols advertises T=1; pass ATR verbatim

                self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + atr_len]
                    .copy_from_slice(&atr[..atr_len]);

                self.tx_len = CCID_HEADER_SIZE + atr_len;
                defmt::info!("CCID: PowerOn success, ATR len={}", atr_len);
            }
            Err(_e) => {
                defmt::error!("CCID: PowerOn failed");
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

    /// Handle PC_to_RDR_ResetParameters — reset to default T=0 parameters
    ///
    /// Reference: CCID Rev 1.1 §6.1.6 - ResetParameters
    ///
    /// Request structure (10 bytes):
    /// - bMessageType: 0x6D
    /// - dwLength: 0x00000000 (no data)
    /// - bSlot: Slot number
    /// - bSeq: Sequence number
    /// - abRFU: 3 bytes reserved
    ///
    /// Response: RDR_to_PC_Parameters (0x82) per §6.2.3
    /// - bProtocolNum: 0x00 (T=0)
    /// - abProtocolData: Default T=0 parameters (Fi=372, Di=1)
    ///
    /// Reference implementation: osmo-ccid-firmware ccid_device.c
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
        self.tx_buffer[9] = 0; // bProtocolNum T=0
        self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + 5].copy_from_slice(&params);
        self.tx_len = CCID_HEADER_SIZE + 5;
    }

    /// Handle PC_to_RDR_SetDataRateAndClockFrequency (CCID 6.1.14)
    ///
    /// Reference: CCID Rev 1.1 §6.1.14 - SetDataRateAndClockFrequency
    ///
    /// Request structure (18 bytes):
    /// - bMessageType: 0x73
    /// - dwLength: 0x00000008 (8 bytes data)
    /// - bSlot: Slot number
    /// - bSeq: Sequence number
    /// - abRFU: 3 bytes reserved
    /// - dwClockFrequency (offset 10-13): Clock frequency in Hz
    /// - dwDataRate (offset 14-17): Data rate in bps
    ///
    /// Response: RDR_to_PC_DataRateAndClockFrequency (0x84)
    /// - dwLength: 0x00000008
    /// - dwClockFrequency: Actual clock frequency set
    /// - dwDataRate: Actual data rate set
    fn handle_set_data_rate_and_clock(&mut self, seq: u8) {
        const MIN_LEN: usize = 10 + 8; // header + dwClockFrequency + dwDataRate
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

    /// Handle PC_to_RDR_IccClock — bClockCommand at byte 7: 0=restart, 1=stop (CCID 6.1.9)
    ///
    /// Reference: CCID Rev 1.1 §6.1.9 - IccClock
    ///
    /// Request structure (10 bytes):
    /// - bMessageType: 0x6E
    /// - dwLength: 0x00000000 (no data)
    /// - bSlot: Slot number
    /// - bSeq: Sequence number
    /// - bClockCommand: 0x00=Restart clock, 0x01=Stop clock
    /// - abRFU: 2 bytes reserved
    ///
    /// Response: RDR_to_PC_SlotStatus (0x81) per §6.2.2
    /// - bClockStatus: 0x00=clock running, 0x01=clock stopped
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
        let b_clock_status: u8 = if enable { 0x00 } else { 0x01 }; // 0=running, 1=stopped
        self.send_slot_status_with_clock(seq, COMMAND_STATUS_NO_ERROR, icc, 0, b_clock_status);
    }

    /// Handle PC_to_RDR_IccPowerOff command
    ///
    /// Reference: CCID Rev 1.1 §6.1.2 - IccPowerOff
    ///
    /// Request structure (10 bytes):
    /// - bMessageType: 0x63
    /// - dwLength: 0x00000000 (no data)
    /// - bSlot: Slot number
    /// - bSeq: Sequence number
    /// - abRFU: 3 bytes reserved
    ///
    /// Response: RDR_to_PC_SlotStatus (0x81) per §6.2.2
    /// - bStatus: bmICCStatus=0x01 (present inactive), bmCommandStatus=0x00
    /// - bClockStatus: 0x00 (clock running)
    fn handle_power_off(&mut self, seq: u8) {
        self.driver.power_off();
        self.slot_state = SlotState::PresentInactive;
        self.current_protocol = 0;

        self.tx_buffer[0] = RDR_TO_PC_SLOTSTATUS;
        self.tx_buffer[1..5].copy_from_slice(&0u32.to_le_bytes()); // dwLength = 0
        self.tx_buffer[5] = 0; // slot
        self.tx_buffer[6] = seq;
        self.tx_buffer[7] =
            Self::build_status(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_INACTIVE);
        self.tx_buffer[8] = 0; // bError
        self.tx_buffer[9] = 0; // bClockStatus

        self.tx_len = CCID_HEADER_SIZE;
        defmt::info!("CCID: PowerOff");
    }

    /// Handle PC_to_RDR_GetSlotStatus command
    ///
    /// Reference: CCID Rev 1.1 §6.1.3 - GetSlotStatus
    ///
    /// Request structure (10 bytes):
    /// - bMessageType: 0x65
    /// - dwLength: 0x00000000 (no data)
    /// - bSlot: Slot number
    /// - bSeq: Sequence number
    /// - abRFU: 3 bytes reserved
    ///
    /// Response: RDR_to_PC_SlotStatus (0x81) per §6.2.2
    /// - bStatus: bmICCStatus per §6.2.6 (0x00=active, 0x01=inactive, 0x02=absent)
    /// - bmCommandStatus: 0x00=OK, 0x01=failed, 0x02=time extension
    fn handle_get_slot_status(&mut self, seq: u8) {
        let icc_status = self.get_icc_status();
        defmt::info!(
            "GetSlotStatus: slot_state={} icc={}",
            self.slot_state as u8,
            icc_status
        );
        self.send_slot_status(seq, COMMAND_STATUS_NO_ERROR, icc_status, 0);
    }

    /// Handle PC_to_RDR_XfrBlock command (Short APDU level - route to T=0 or T=1 engine)
    ///
    /// Reference: CCID Rev 1.1 §6.1.4 - XfrBlock
    ///
    /// Request structure (10+ bytes):
    /// - bMessageType: 0x6F
    /// - dwLength: APDU data length
    /// - bSlot: Slot number
    /// - bSeq: Sequence number
    /// - bBWI: Block Waiting Integer (ignored for sync operation)
    /// - wLevelParameter: Level parameter (ignored for Short APDU level)
    /// - abData: APDU command bytes
    ///
    /// Response: RDR_to_PC_DataBlock (0x80) per §6.2.1
    /// - dwLength: Response length
    /// - abData: APDU response (SW1 SW2 + optional data)
    ///
    /// Error codes (Table 6.2-2):
    /// - 0xFE ICC_MUTE: Card not active or communication failed
    /// - 0x07: Extended APDU not supported (max 261 bytes)
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

        if data_len > 261 {
            defmt::warn!("CCID: XfrBlock Extended APDU rejected");
            self.send_slot_status(seq, COMMAND_STATUS_FAILED, ICC_STATUS_PRESENT_ACTIVE, 0x07);
            return;
        }
        if data_len > MAX_CCID_MESSAGE_LENGTH - CCID_HEADER_SIZE {
            self.send_slot_status(seq, COMMAND_STATUS_FAILED, ICC_STATUS_PRESENT_ACTIVE, 0x07);
            return;
        }

        let apdu = &self.rx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + data_len];
        defmt::info!(
            "CCID: XfrBlock APDU len={} first={=[u8]:x}",
            data_len,
            &apdu[..data_len.min(12)]
        );
        let mut response_buf = [0u8; MAX_CCID_MESSAGE_LENGTH - CCID_HEADER_SIZE];
        let resp_len: usize;

        match self.driver.transmit_apdu(apdu, &mut response_buf) {
            Ok(len) => {
                resp_len = len;
                defmt::info!("CCID: XfrBlock OK resp_len={}", resp_len);
            }
            Err(_e) => {
                defmt::error!("CCID: XfrBlock failed (card timeout or protocol error)");
                self.send_slot_status(seq, COMMAND_STATUS_FAILED, ICC_STATUS_PRESENT_ACTIVE, 0xFF);
                return;
            }
        }

        let resp_len = resp_len.min(response_buf.len());

        self.tx_buffer[0] = RDR_TO_PC_DATABLOCK;
        self.tx_buffer[1..5].copy_from_slice(&(resp_len as u32).to_le_bytes());
        self.tx_buffer[5] = 0; // slot
        self.tx_buffer[6] = seq;
        self.tx_buffer[7] = Self::build_status(COMMAND_STATUS_NO_ERROR, ICC_STATUS_PRESENT_ACTIVE);
        self.tx_buffer[8] = 0; // bError
        self.tx_buffer[9] = 0; // bChainParameter

        self.tx_buffer[CCID_HEADER_SIZE..CCID_HEADER_SIZE + resp_len]
            .copy_from_slice(&response_buf[..resp_len]);

        self.tx_len = CCID_HEADER_SIZE + resp_len;
        defmt::debug!("CCID: XfrBlock success, resp_len={}", resp_len);
    }

    /// Handle PC_to_RDR_GetParameters command (real values from AtrParams)
    ///
    /// Reference: CCID Rev 1.1 §6.1.5 - GetParameters
    ///
    /// Request structure (10 bytes):
    /// - bMessageType: 0x6C
    /// - dwLength: 0x00000000 (no data)
    /// - bSlot: Slot number
    /// - bSeq: Sequence number
    /// - abRFU: 3 bytes reserved
    ///
    /// Response: RDR_to_PC_Parameters (0x82) per §6.2.3
    /// - bProtocolNum: 0x00=T=0, 0x01=T=1
    /// - abProtocolData: Per Table 6.2-3
    ///   - T=0 (5 bytes): bmFindexDindex, bmTCCKST0, bGuardTimeT0, bWaitingIntegerT0, bClockStop
    ///   - T=1 (7 bytes): bmFindexDindex, bmTCCKST1, bGuardTimeT1, bWaitingIntegersT1, bClockStop, bIFSC, bNadValue
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
            self.tx_buffer[9] = 0; // bClockStatus: 0x00 for T=1 (clock always running)
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

    /// Handle PC_to_RDR_SetParameters command
    ///
    /// Reference: CCID Rev 1.1 §6.1.7 - SetParameters
    ///
    /// Request structure (10+ bytes):
    /// - bMessageType: 0x61
    /// - dwLength: Protocol data length (5 for T=0, 7 for T=1)
    /// - bSlot: Slot number
    /// - bSeq: Sequence number
    /// - bProtocolNum: 0x00=T=0, 0x01=T=1
    /// - abRFU: 2 bytes reserved
    /// - abProtocolData: Per Table 6.2-3
    ///
    /// Response: RDR_to_PC_Parameters (0x82) per §6.2.3
    ///
    /// Note: libccid sends protocol data WITHOUT bProtocolNum prefix (implementation quirk).
    /// We infer protocol from dwLength: 5=T=0, 7=T=1.
    fn handle_set_parameters(&mut self, seq: u8) {
        // CCID header: [mt][dwLength 4][bSlot][bSeq][bBWI][wLevel 2]
        // Reference: CCID Rev 1.1 Table 6.2-3
        //
        // NOTE: libccid sends protocol data structure WITHOUT bProtocolNum prefix!
        // The spec says abData[0] = bProtocolNum, but libccid sends:
        //   - For T=0: 5 bytes of T=0 params (no bProtocolNum prefix)
        //   - For T=1: 7 bytes of T=1 params (no bProtocolNum prefix)
        // We infer protocol from dwLength: 5=T=0, 7=T=1
        let data_len = u32::from_le_bytes([
            self.rx_buffer[1],
            self.rx_buffer[2],
            self.rx_buffer[3],
            self.rx_buffer[4],
        ]) as usize;

        // Infer protocol from data length
        let requested_protocol = match data_len {
            5 => 0, // T=0
            7 => 1, // T=1
            _ => {
                defmt::error!("CCID: SetParameters invalid dwLength={}", data_len);
                self.send_slot_status(seq, COMMAND_STATUS_FAILED, self.get_icc_status(), 0x07);
                return;
            }
        };

        // Accept the protocol
        self.driver.set_protocol(requested_protocol);
        self.current_protocol = requested_protocol;

        // Return real parameters from AtrParams (same as GetParameters)
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
            self.tx_buffer[9] = 0; // bClockStatus: 0x00 for T=1 (clock always running)
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

    /// Handle PC_to_RDR_Secure command (PIN verification/modification)
    ///
    /// Reference: CCID Rev 1.1 §6.1.11 (PIN Verify), §6.1.12 (PIN Modify)
    ///
    /// Request structure (10+ bytes):
    /// - bMessageType: 0x69
    /// - dwLength: PIN data structure length
    /// - bSlot: Slot number
    /// - bSeq: Sequence number
    /// - bBWI: Block Waiting Integer
    /// - wLevelParameter: Level parameter
    /// - bmPINOperation (offset 10): 0x00=Verify, 0x01=Modify
    /// - PIN Data Structure (offset 11+): Per §6.1.11 (Verify) or §6.1.12 (Modify)
    ///
    /// PIN Verify Data Structure (§6.1.11):
    /// - bTimeOut, bmFormatString, bmPINBlockString, bmPINLengthFormat
    /// - wPINMaxExtraDigit (max|min), bEntryValidationCondition
    /// - bNumberMessage, wLangId, bMsgIndex, bTeoPrologue, abPINApdu
    ///
    /// PIN Modify Data Structure (§6.1.12):
    /// - Same as Verify + bConfirmPIN, bInsertPosition, bReplacePosition
    ///
    /// Response: RDR_to_PC_DataBlock (0x80) per §6.2.1
    /// - Error codes: PIN_CANCELLED (0xEF), PIN_TIMEOUT (0xF0), CMD_ABORTED (0xFF)
    ///
    /// Implementation note: Deferred response - PIN entry happens on touchscreen.
    fn handle_secure(&mut self, seq: u8) {
        // 1. Check slot state - must have active card
        if self.slot_state != SlotState::PresentActive {
            self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_SLOT_BUSY);
            return;
        }

        // 2. Parse bmPINOperation from data area (CCID Rev 1.1 spec section 6.2.7)
        // CCID header is 10 bytes: dwLength(4) + bSlot(1) + bSeq(1) + bBWI(1) + wLevelParameter(2) + RFU(1)
        // Data area starts at offset 10, first byte is bmPINOperation
        // Need at least 11 bytes: 10-byte CCID header + 1 byte PIN operation
        if self.rx_len <= 10 {
            self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            return;
        }

        let pin_operation = self.rx_buffer[10]; // bmPINOperation - first byte of data area

        match pin_operation {
            0x00 => {
                // PIN Verify - parse PIN Verify Data Structure (CCID Rev 1.1 Section 6.1.11)
                // Data area: bmPINOperation(1) + PIN Verify Data Structure
                // PIN Verify Data Structure starts at offset 11 (after bmPINOperation)
                let pin_data = &self.rx_buffer[11..self.rx_len];

                match PinVerifyParams::parse(pin_data) {
                    Some(params) => {
                        // Store for deferred response - PIN entry will happen on display/touch
                        self.secure_state = SecureState::WaitingForPinVerify { seq, params };
                        defmt::debug!("CCID: PIN Verify - waiting for PIN entry");
                        // No immediate response - will send RDR_to_PC_DataBlock after PIN entry
                    }
                    None => {
                        defmt::warn!("CCID: PIN Verify parse failed");
                        self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
                    }
                }
            }
            0x01 => {
                // PIN Modify - parse PIN Modify Data Structure (CCID Rev 1.1 Section 6.1.12)
                // Data area: bmPINOperation(1) + PIN Modify Data Structure
                // PIN Modify Data Structure starts at offset 11 (after bmPINOperation)
                let pin_data = &self.rx_buffer[11..self.rx_len];

                match PinModifyParams::parse(pin_data) {
                    Some(params) => {
                        // Store for deferred response - PIN entry will happen on display/touch
                        self.secure_state = SecureState::WaitingForPinModify { seq, params };
                        defmt::debug!("CCID: PIN Modify - waiting for PIN entry");
                    }
                    None => {
                        defmt::warn!("CCID: PIN Modify parse failed");
                        self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
                    }
                }
            }
            _ => {
                defmt::warn!("CCID: Unknown PIN operation: {}", pin_operation);
                self.send_err_resp(PC_TO_RDR_SECURE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
        }
    }

    /// Handle PC_to_RDR_Escape command
    ///
    /// Gemalto readers (VID 0x08E6) need to handle escape 0x6A
    /// (GET_FIRMWARE_FEATURES) to return a valid GEMALTO_FIRMWARE_FEATURES
    /// struct. Without this, libccid treats the reader as buggy and
    /// triggers the modify PIN workaround (bNumberMessageFix defaults to 0).
    ///
    /// GEMALTO_FIRMWARE_FEATURES struct (from libccid src/ccid.c):
    ///   [0] bNumberMessageFix: u8  — set to 1 to suppress workaround
    ///   [1] bPPDUSupportOverXferBlock: u8
    ///   [2] wLcdLayout: u16 (LE)
    ///   [4] bPINSupport: u8
    ///   [5] bDisplayStatus: u8
    ///   [6] bEntryStatus: u8
    ///   [7] bVerifyPinStart: u8
    ///   [8] bVerifyPinFinish: u8
    ///   [9] bModifyPinStart: u8
    ///   [10] bModifyPinFinish: u8
    ///   [11] bGetKeyPressed: u8
    ///   [12] bWriteDisplay: u8
    ///   [13] bSetSpeMessage: u8
    ///   [14] bPPDUSupportOverEscape: u8
    fn handle_escape(&mut self, seq: u8) {
        let data_len = u32::from_le_bytes([
            self.rx_buffer[1],
            self.rx_buffer[2],
            self.rx_buffer[3],
            self.rx_buffer[4],
        ]) as usize;
        let is_gemalto = CURRENT_PROFILE.vendor_id == 0x08E6;
        let is_firmware_features_query = data_len >= 1 && self.rx_buffer[CCID_HEADER_SIZE] == 0x6A;

        if is_gemalto && is_firmware_features_query {
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
            defmt::debug!("CCID: Escape 0x6A -> GEMALTO_FIRMWARE_FEATURES response");
        } else {
            defmt::debug!("CCID: Escape command (stub - vendor-specific)");
            self.send_err_resp(PC_TO_RDR_ESCAPE, seq, CCID_ERR_CMD_NOT_SUPPORTED);
        }
    }

    /// Check if PIN entry is currently active
    /// Returns true if a PC_to_RDR_Secure command is pending PIN entry
    pub fn is_pin_entry_active(&self) -> bool {
        matches!(
            self.secure_state,
            SecureState::WaitingForPinVerify { .. } | SecureState::WaitingForPinModify { .. }
        )
    }

    /// Check if PIN verify entry is active
    pub fn is_pin_verify_active(&self) -> bool {
        matches!(self.secure_state, SecureState::WaitingForPinVerify { .. })
    }

    /// Check if PIN modify entry is active
    pub fn is_pin_modify_active(&self) -> bool {
        matches!(self.secure_state, SecureState::WaitingForPinModify { .. })
    }

    /// Take the secure PIN verify parameters, resetting state to Idle
    /// Returns (seq, params) if PIN verify was active, None otherwise
    /// This is called by the main loop when starting PIN entry UI
    pub fn take_secure_params(&mut self) -> Option<(u8, PinVerifyParams)> {
        if let SecureState::WaitingForPinVerify { seq, params } =
            core::mem::replace(&mut self.secure_state, SecureState::Idle)
        {
            Some((seq, params))
        } else {
            None
        }
    }

    /// Take the secure PIN modify parameters, resetting state to Idle
    /// Returns (seq, params) if PIN modify was active, None otherwise
    pub fn take_secure_modify_params(&mut self) -> Option<(u8, PinModifyParams)> {
        if let SecureState::WaitingForPinModify { seq, params } =
            core::mem::replace(&mut self.secure_state, SecureState::Idle)
        {
            Some((seq, params))
        } else {
            None
        }
    }

    /// Complete PIN entry with result and optional APDU response
    /// Sends RDR_to_PC_DataBlock response to host
    /// This is called by the main loop after PIN entry is complete
    ///
    /// # Arguments
    /// * `seq` - CCID sequence number from original PC_to_RDR_Secure request
    /// * `result` - PIN entry result (Success/Cancelled/Timeout/InvalidLength)
    /// * `apdu_response` - Card response bytes on success (SW1 SW2 + optional data)
    pub fn complete_pin_entry(&mut self, seq: u8, result: PinResult, apdu_response: Option<&[u8]>) {
        defmt::debug!(
            "CCID: PIN entry complete - seq={}, result={:?}",
            seq,
            result
        );

        let icc_status = self.get_icc_status();

        match result {
            PinResult::Success => {
                // APDU was sent to card successfully
                if let Some(resp) = apdu_response {
                    self.send_data_block_response(
                        seq,
                        resp,
                        COMMAND_STATUS_NO_ERROR,
                        icc_status,
                        0,
                    );
                } else {
                    // No response data - shouldn't happen but handle gracefully
                    self.send_data_block_response(seq, &[], COMMAND_STATUS_NO_ERROR, icc_status, 0);
                }
            }
            PinResult::Cancelled => {
                // User cancelled - send error response
                defmt::warn!("CCID: PIN entry cancelled by user");
                self.send_data_block_response(
                    seq,
                    &[],
                    COMMAND_STATUS_FAILED,
                    icc_status,
                    CCID_ERR_PIN_CANCELLED,
                );
            }
            PinResult::Timeout => {
                // Timeout - send error response
                defmt::warn!("CCID: PIN entry timed out");
                self.send_data_block_response(
                    seq,
                    &[],
                    COMMAND_STATUS_FAILED,
                    icc_status,
                    CCID_ERR_PIN_TIMEOUT,
                );
            }
            PinResult::InvalidLength => {
                // Invalid PIN length - send error response
                defmt::warn!("CCID: Invalid PIN length");
                self.send_data_block_response(
                    seq,
                    &[],
                    COMMAND_STATUS_FAILED,
                    icc_status,
                    CCID_ERR_CMD_ABORTED,
                );
            }
        }

        // Ensure state is reset to Idle
        self.secure_state = SecureState::Idle;
    }

    /// Check if a card is currently present
    /// This is used by the main loop for display updates
    pub fn is_card_present(&self) -> bool {
        self.driver.is_card_present()
    }

    /// Check if a card is present AND powered on (active)
    /// This is used to determine if APDU communication is possible
    pub fn is_card_active(&self) -> bool {
        self.slot_state == SlotState::PresentActive
    }

    /// Store PIN entry result from touchscreen
    /// This is called by main loop after PIN entry completes
    /// Params are stored to avoid race condition with secure_state being cleared
    #[cfg(feature = "display")]
    pub fn set_pin_result(
        &mut self,
        seq: u8,
        result: PinResult,
        buffer: PinBuffer,
        params: PinVerifyParams,
    ) {
        defmt::debug!(
            "CCID: Storing PIN result - seq={}, result={:?}, pin_len={}",
            seq,
            result,
            buffer.len()
        );
        self.pin_result_pending = Some((seq, result, buffer, params));
    }

    /// Process pending PIN result - transmit APDU to card
    /// This is called by main loop each iteration
    #[cfg(feature = "display")]
    pub fn process_pin_result(&mut self) {
        let Some((seq, result, buffer, params)) = self.pin_result_pending.take() else {
            return;
        };

        defmt::debug!(
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

                defmt::debug!(
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
                        defmt::debug!("CCID: Transmitting APDU, len={}", apdu_len);
                        match self
                            .driver
                            .transmit_apdu(&apdu[..apdu_len], &mut self.response_buffer)
                        {
                            Ok(resp_len) => {
                                defmt::info!("CCID: Card responded, len={}", resp_len);
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
                                defmt::warn!("CCID: Card transmit failed");
                                self.complete_pin_entry(seq, PinResult::Cancelled, None);
                            }
                        }
                    }
                    Err(_e) => {
                        defmt::warn!("CCID: APDU build failed");
                        self.complete_pin_entry(seq, PinResult::InvalidLength, None);
                    }
                }
            }
            PinResult::Cancelled | PinResult::Timeout | PinResult::InvalidLength => {
                defmt::debug!("CCID: PIN entry failed with {:?}", result);
                self.complete_pin_entry(seq, result, None);
            }
        }
    }

    /// Store PIN modify entry result from touchscreen
    /// This is called by main loop after PIN modify entry completes
    #[cfg(feature = "display")]
    pub fn set_pin_modify_result(
        &mut self,
        seq: u8,
        result: PinResult,
        old_buffer: PinBuffer,
        new_buffer: PinBuffer,
        params: PinModifyParams,
    ) {
        defmt::debug!(
            "CCID: Storing PIN modify result - seq={}, result={:?}, old_len={}, new_len={}",
            seq,
            result,
            old_buffer.len(),
            new_buffer.len()
        );
        self.pin_modify_result_pending = Some((seq, result, old_buffer, new_buffer, params));
    }

    /// Process pending PIN modify result - transmit CHANGE REFERENCE DATA APDU to card
    /// This is called by main loop each iteration
    /// Per ISO 7816-4 §7.5.7: CHANGE REFERENCE DATA (INS=0x24)
    #[cfg(feature = "display")]
    pub fn process_pin_modify_result(&mut self) {
        let Some((seq, result, old_buffer, new_buffer, params)) =
            self.pin_modify_result_pending.take()
        else {
            return;
        };

        defmt::debug!(
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

                defmt::debug!(
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
                        defmt::debug!("CCID: Transmitting CHANGE APDU, len={}", apdu_len);
                        match self
                            .driver
                            .transmit_apdu(&apdu[..apdu_len], &mut self.response_buffer)
                        {
                            Ok(resp_len) => {
                                defmt::info!("CCID: Card responded to CHANGE, len={}", resp_len);
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
                                defmt::warn!("CCID: Card transmit failed for CHANGE");
                                self.complete_pin_entry(seq, PinResult::Cancelled, None);
                            }
                        }
                    }
                    Err(_e) => {
                        defmt::warn!("CCID: CHANGE APDU build failed");
                        self.complete_pin_entry(seq, PinResult::InvalidLength, None);
                    }
                }
            }
            PinResult::Cancelled | PinResult::Timeout | PinResult::InvalidLength => {
                defmt::debug!("CCID: PIN modify entry failed with {:?}", result);
                self.complete_pin_entry(seq, result, None);
            }
        }
    }

    /// Send RDR_to_PC_DataBlock response
    ///
    /// Reference: CCID Rev 1.1 §6.2.1 - RDR_to_PC_DataBlock
    ///
    /// Response structure (10+ bytes):
    /// - bMessageType: 0x80
    /// - dwLength: Data length
    /// - bSlot: Slot number
    /// - bSeq: Sequence number (from request)
    /// - bStatus: bmICCStatus | bmCommandStatus per §6.2.6
    /// - bError: Error code per Table 6.2-2 (0 on success)
    /// - bChainParameter: Chain parameter (0 for Short APDU level)
    /// - abData: Response data
    fn send_data_block_response(
        &mut self,
        seq: u8,
        data: &[u8],
        cmd_status: u8,
        icc_status: u8,
        error: u8,
    ) {
        let data_len = data.len() as u32;

        // Build CCID header
        self.tx_buffer[0] = RDR_TO_PC_DATABLOCK;
        self.tx_buffer[1..5].copy_from_slice(&data_len.to_le_bytes());
        self.tx_buffer[5] = 0; // Slot
        self.tx_buffer[6] = seq;
        self.tx_buffer[7] = Self::build_status(cmd_status, icc_status);
        self.tx_buffer[8] = error;
        self.tx_buffer[9] = 0; // Clock status

        // Copy data after header
        let header_len = CCID_HEADER_SIZE;
        if data.len() + header_len <= self.tx_buffer.len() {
            self.tx_buffer[header_len..header_len + data.len()].copy_from_slice(data);
            self.tx_len = header_len + data.len();
        } else {
            // Data too large - truncate (shouldn't happen with SW1 SW2 responses)
            defmt::warn!("CCID: DataBlock response truncated");
            let truncated_len = self.tx_buffer.len() - header_len;
            self.tx_buffer[header_len..].copy_from_slice(&data[..truncated_len]);
            self.tx_len = self.tx_buffer.len();
        }

        defmt::trace!(
            "CCID: Sending DataBlock seq={}, len={}, status={}, error={}",
            seq,
            self.tx_len,
            self.tx_buffer[7],
            error
        );
    }

    /// Send a SlotStatus response
    ///
    /// Reference: CCID Rev 1.1 §6.2.2 - RDR_to_PC_SlotStatus
    ///
    /// Response structure (10 bytes):
    /// - bMessageType: 0x81
    /// - dwLength: 0x00000000 (no data)
    /// - bSlot: Slot number
    /// - bSeq: Sequence number (from request)
    /// - bStatus: bmICCStatus | bmCommandStatus per §6.2.6
    /// - bError: Error code per Table 6.2-2 (0 on success)
    /// - bClockStatus: 0x00=running, 0x01=stopped (per §6.1.9 response)
    fn send_slot_status(&mut self, seq: u8, cmd_status: u8, icc_status: u8, error: u8) {
        self.send_slot_status_with_clock(seq, cmd_status, icc_status, error, 0);
    }

    /// Send a SlotStatus response with bClockStatus (for IccClock response)
    ///
    /// Reference: CCID Rev 1.1 §6.2.2 - RDR_to_PC_SlotStatus
    /// Reference: CCID Rev 1.1 §6.1.9 - IccClock (bClockStatus field)
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
        self.tx_buffer[5] = 0; // slot
        self.tx_buffer[6] = seq;
        self.tx_buffer[7] = Self::build_status(cmd_status, icc_status);
        self.tx_buffer[8] = error;
        self.tx_buffer[9] = b_clock_status;

        self.tx_len = CCID_HEADER_SIZE;
    }

    /// Try to send pending response (64-byte chunks + ZLP when exact multiple of 64)
    fn try_send(&mut self) {
        if self.needs_zlp {
            match self.ep_in.write(&[]) {
                Ok(_) => {
                    self.needs_zlp = false;
                    self.cmd_busy = false;
                }
                Err(UsbError::WouldBlock) => {}
                Err(_) => {
                    self.needs_zlp = false;
                    self.cmd_busy = false;
                }
            }
            return;
        }
        if self.tx_len == 0 {
            return;
        }
        let remaining = self.tx_len - self.tx_offset;
        let chunk_size = remaining.min(PACKET_SIZE);
        let chunk = &self.tx_buffer[self.tx_offset..self.tx_offset + chunk_size];
        match self.ep_in.write(chunk) {
            Ok(n) => {
                self.tx_offset += n;
                if self.tx_offset >= self.tx_len {
                    if self.tx_len % PACKET_SIZE == 0 {
                        self.needs_zlp = true;
                    } else {
                        self.cmd_busy = false;
                    }
                    self.tx_len = 0;
                    self.tx_offset = 0;
                }
            }
            Err(UsbError::WouldBlock) => {}
            Err(_) => {
                self.tx_len = 0;
                self.tx_offset = 0;
                self.cmd_busy = false;
            }
        }
    }
}

impl<'bus, Bus: UsbBus, D: SmartcardDriver> UsbClass<Bus> for CcidClass<'bus, Bus, D> {
    fn get_configuration_descriptors(&self, writer: &mut DescriptorWriter) -> Result<()> {
        // Write interface descriptor
        writer.interface_alt(
            self.interface,
            0, // bAlternateSetting
            CLASS_CCID,
            SUBCLASS_NONE,
            PROTOCOL_BULK,
            None, // No string description
        )?;

        // Write CCID class descriptor
        writer.write(DESCRIPTOR_TYPE_CCID, &CCID_CLASS_DESCRIPTOR_DATA)?;

        // Write endpoint descriptors (Bulk IN, Bulk OUT, Interrupt IN)
        writer.endpoint(&self.ep_in)?;
        writer.endpoint(&self.ep_out)?;
        writer.endpoint(&self.ep_int)?;

        Ok(())
    }

    fn poll(&mut self) {
        let present_now = self.driver.is_card_present();
        if present_now != self.card_present_last {
            defmt::info!(
                "Card state change: {} -> {}",
                self.card_present_last,
                present_now
            );
            self.card_present_last = present_now;
            self.send_notify_slot_change(present_now, true);
            if !present_now {
                // Card removed - must power off driver and reset state
                // to avoid crash/inconsistency on reinsert (CCID Rev 1.1 §6.3)
                defmt::info!("Card removed, powering off driver");
                self.driver.power_off();
                self.slot_state = SlotState::Absent;
                self.cmd_busy = false; // Cancel any pending command
                self.rx_len = 0; // Clear any pending receive data
                self.secure_state = SecureState::Idle; // Cancel any PIN entry
                #[cfg(feature = "display")]
                {
                    self.pin_result_pending = None;
                }
            } else {
                self.slot_state = SlotState::PresentInactive;
            }
        }

        // Try to send any pending response first
        self.try_send();

        // Read from OUT endpoint
        let mut temp_buf = [0u8; PACKET_SIZE];
        match self.ep_out.read(&mut temp_buf) {
            Ok(len) => {
                defmt::info!("CCID: USB received {} bytes: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                    len,
                    temp_buf[0], temp_buf[1], temp_buf[2], temp_buf[3],
                    temp_buf[4], temp_buf[5], temp_buf[6], temp_buf[7],
                    temp_buf[8], temp_buf[9]);
                let remaining = self.rx_buffer.len() - self.rx_len;
                let copy_len = len.min(remaining);
                self.rx_buffer[self.rx_len..self.rx_len + copy_len]
                    .copy_from_slice(&temp_buf[..copy_len]);
                self.rx_len += copy_len;
                if self.rx_len >= CCID_HEADER_SIZE {
                    let msg_len = u32::from_le_bytes([
                        self.rx_buffer[1],
                        self.rx_buffer[2],
                        self.rx_buffer[3],
                        self.rx_buffer[4],
                    ]) as usize;
                    let total_len = CCID_HEADER_SIZE + msg_len;
                    if self.rx_len >= total_len {
                        defmt::info!(
                            "CCID: rx_len={} total_len={} calling handle_message",
                            self.rx_len,
                            total_len
                        );
                        self.handle_message();
                        self.rx_len = 0;
                    }
                }
            }
            Err(UsbError::WouldBlock) => {}
            Err(_e) => {
                defmt::error!("CCID: read error");
                self.rx_len = 0;
            }
        }

        // Try to send again after processing
        self.try_send();
    }

    fn endpoint_out(&mut self, addr: EndpointAddress) {
        if addr == self.ep_out.address() {
            // Data received on OUT endpoint, will be processed in poll()
        }
    }

    fn endpoint_in_complete(&mut self, addr: EndpointAddress) {
        if addr == self.ep_in.address() {
            // Previous IN transfer complete, try to send more if pending
            self.try_send();
        }
    }

    fn control_in(&mut self, transfer: ControlIn<Bus>) {
        let request = transfer.request();

        // Only handle class requests to this interface
        if request.index as u8 != u8::from(self.interface) {
            return;
        }

        if request.request_type != usb_device::control::RequestType::Class {
            return;
        }

        // Reference: CCID Rev 1.1 §5.3 - Class-specific Requests
        match request.request {
            // §5.3.2 GET_CLOCK_FREQUENCIES - Returns supported clock frequencies
            REQUEST_GET_CLOCK_FREQUENCIES => {
                transfer.accept_with(&CLOCK_FREQUENCY_KHZ).ok();
            }
            // §5.3.3 GET_DATA_RATES - Returns supported data rates
            REQUEST_GET_DATA_RATES => {
                transfer.accept_with(&DATA_RATE_BPS).ok();
            }
            _ => {
                transfer.reject().ok();
            }
        }
    }

    fn control_out(&mut self, transfer: ControlOut<Bus>) {
        let request = transfer.request();

        // Only handle class requests to this interface
        if request.index as u8 != u8::from(self.interface) {
            return;
        }

        if request.request_type != usb_device::control::RequestType::Class {
            return;
        }

        // Reference: CCID Rev 1.1 §5.3 - Class-specific Requests
        match request.request {
            // §5.3.1 ABORT - Abort command (wValue: slot in low byte, seq in high byte)
            REQUEST_ABORT => {
                let _slot = (request.value & 0xFF) as u8;
                let _seq = ((request.value >> 8) & 0xFF) as u8;
                transfer.accept().ok();
            }
            _ => {
                transfer.reject().ok();
            }
        }
    }
}
