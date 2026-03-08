//! USB CCID Class implementation for smartcard reader mode
//!
//! This module implements the USB Chip/Smart Card Interface Device (CCID) protocol
//! as defined in the CCID Specification Rev 1.1 for smartcard reader functionality.

use usb_device::class_prelude::*;
use usb_device::endpoint::{In, Out};
use usb_device::{Result, UsbError};

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
pub const COMMAND_STATUS_TIME_EXTENSION: u8 = 0x80;

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

// ============================================================================
/// CCID class functional descriptor DATA (52 bytes, without length and type)
/// This is passed to writer.write(DESCRIPTOR_TYPE_CCID, &CCID_CLASS_DESCRIPTOR_DATA)
/// which will prepend the length (54) and type (0x21)
pub const CCID_CLASS_DESCRIPTOR_DATA: [u8; 52] = [
    // bcdCCID: CCID Class Spec release number 1.10 (little-endian)
    0x10, 0x01, // bMaxSlotIndex: Highest available slot (0 = single slot)
    0x00, // bVoltageSupport: 5V, 3V, 1.8V (bits 0,1,2)
    0x07, // dwProtocols: T=0 and T=1
    0x03, 0x00, 0x00, 0x00, // dwDefaultClock: 4 MHz = 4,000,000 Hz (little-endian)
    0x00, 0x2D, 0x3D, 0x00, // dwMaximumClock: 20 MHz = 20,000,000 Hz
    0x80, 0x84, 0x31, 0x01, // bNumClockSupported: 0 (use default/maximum)
    0x00, // dwDataRate: 10752 bps (default for T=0)
    0x00, 0x2A, 0x00, 0x00, // dwMaxDataRate: 344086 bps
    0x36, 0x41, 0x05, 0x00, // bNumDataRatesSupported: 0 (use default/maximum)
    0x00, // dwMaxIFSD: 254 bytes (maximum IFSD for T=1, not used for T=0)
    0xFE, 0x00, 0x00, 0x00, // dwSynchProtocols: None
    0x00, 0x00, 0x00, 0x00, // dwMechanical: No special characteristics
    0x00, 0x00, 0x00, 0x00,
    // dwFeatures: 0x000207B2 (APDU level + auto params + clock stop + NAD + auto IFSD)
    // - 0x02: Automatic parameter configuration based on ATR
    // - 0x10: Automatic ICC clock frequency change
    // - 0x20: Automatic baud rate change (Fi/Di)
    // - 0x80: Automatic PPS made by CCID
    // - 0x0100: CCID can set ICC in clock stop mode
    // - 0x0200: NAD value other than 00 accepted (T=1)
    // - 0x0400: Automatic IFSD exchange as first exchange (T=1)
    // - 0x00020000: Short APDU level exchange
    0xB2, 0x07, 0x02, 0x00, // dwMaxCCIDMessageLength: 271 bytes (10 header + 261 data)
    0x0F, 0x01, 0x00, 0x00, // bClassGetResponse: 0xFF (echo)
    0xFF, // bClassEnvelope: 0xFF (echo)
    0xFF, // wLcdLayout: 0x0000 (no LCD)
    0x00, 0x00, // bPINSupport: 0x00 (no PIN support)
    0x00, // bMaxCCIDBusySlots: 1
    0x01,
];

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
/// CCID Bulk-OUT message header (10 bytes)
/// Note: Header fields are parsed manually from rx_buffer to avoid packed struct issues

// ============================================================================
// CcidClass Implementation
// ============================================================================

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
        Self {
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
            current_protocol: 0, // Default to T=0
            atr_params: AtrParams::default(),
        }
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
            PC_TO_RDR_ESCAPE => {
                defmt::debug!("CCID: Escape command (stub - vendor-specific)");
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
            PC_TO_RDR_ICC_CLOCK => {
                self.handle_icc_clock(seq);
            }
            PC_TO_RDR_T0_APDU => {
                defmt::debug!("CCID: T0APDU command (stub - TPDU level sufficient)");
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
            }
            PC_TO_RDR_SECURE => {
                defmt::debug!("CCID: Secure command (stub - no PIN hardware)");
                self.send_err_resp(msg_type, seq, CCID_ERR_CMD_NOT_SUPPORTED);
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
    fn handle_power_on(&mut self, seq: u8) {
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

    /// Handle PC_to_RDR_ResetParameters — reset to default T=0 parameters (osmo 6.1.6)
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
    fn handle_get_slot_status(&mut self, seq: u8) {
        let icc_status = self.get_icc_status();
        self.send_slot_status(seq, COMMAND_STATUS_NO_ERROR, icc_status, 0);
    }

    /// Handle PC_to_RDR_XfrBlock command (Short APDU level - route to T=0 or T=1 engine)
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
            self.tx_buffer[9] = 1;
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
    fn handle_set_parameters(&mut self, seq: u8) {
        // bProtocolNum is in b_specific[0] (byte 7 of the CCID message)
        let requested_protocol = self.rx_buffer[7];

        // Debug: dump incoming message header
        defmt::info!("CCID: SetParameters IN: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
            self.rx_buffer[0], self.rx_buffer[1], self.rx_buffer[2], self.rx_buffer[3],
            self.rx_buffer[4], self.rx_buffer[5], self.rx_buffer[6], self.rx_buffer[7],
            self.rx_buffer[8], self.rx_buffer[9]);
        defmt::info!(
            "CCID: SetParameters protocol={} requested",
            requested_protocol
        );

        if requested_protocol > 1 {
            defmt::error!("CCID: Unsupported protocol {}", requested_protocol);
            self.send_slot_status(seq, COMMAND_STATUS_FAILED, self.get_icc_status(), 0x07);
            return;
        }

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
            self.tx_buffer[9] = 1;
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

    /// Send a SlotStatus response
    fn send_slot_status(&mut self, seq: u8, cmd_status: u8, icc_status: u8, error: u8) {
        self.send_slot_status_with_clock(seq, cmd_status, icc_status, error, 0);
    }

    /// Send a SlotStatus response with bClockStatus (for IccClock response)
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
        // Card detect edge detection for NotifySlotChange
        let present_now = self.driver.is_card_present();
        if present_now != self.card_present_last {
            self.card_present_last = present_now;
            self.send_notify_slot_change(present_now, true);
            if !present_now {
                self.slot_state = SlotState::Absent;
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

        match request.request {
            REQUEST_GET_CLOCK_FREQUENCIES => {
                // Return supported clock frequencies
                transfer.accept_with(&CLOCK_FREQUENCY_KHZ).ok();
            }
            REQUEST_GET_DATA_RATES => {
                // Return supported data rates
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

        match request.request {
            REQUEST_ABORT => {
                // Abort command - slot in low byte, seq in high byte of value
                let _slot = (request.value & 0xFF) as u8;
                let _seq = ((request.value >> 8) & 0xFF) as u8;
                // For now, just accept the abort
                transfer.accept().ok();
            }
            _ => {
                transfer.reject().ok();
            }
        }
    }
}
