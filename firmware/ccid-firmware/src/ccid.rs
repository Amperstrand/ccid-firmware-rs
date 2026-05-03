#![cfg(all(target_arch = "arm", target_os = "none"))]
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(clippy::identity_op)]
#![allow(clippy::manual_is_multiple_of)]
//! USB CCID Class implementation — transport layer only
//!
//! Protocol logic lives in ccid_core::CcidMessageHandler.
//! This module provides the USB transport (endpoint I/O, packet reassembly,
//! ZLP handling, interrupt IN) and wraps CcidMessageHandler.

use usb_device::class_prelude::*;
use usb_device::endpoint::{In, Out};
use usb_device::{Result, UsbError};

use ccid_firmware_rs::ccid_core::CcidMessageHandler;
use ccid_firmware_rs::device_profile::CURRENT_PROFILE;
use ccid_protocol::types::{
    CCID_HEADER_SIZE, CLASS_CCID, DESCRIPTOR_TYPE_CCID, PACKET_SIZE, PROTOCOL_BULK, REQUEST_ABORT,
    REQUEST_GET_CLOCK_FREQUENCIES, REQUEST_GET_DATA_RATES, SUBCLASS_NONE,
};

pub const CCID_CLASS_DESCRIPTOR_DATA: [u8; 52] = CURRENT_PROFILE.ccid_descriptor();
pub const CLOCK_FREQUENCY_KHZ: [u8; 4] = [0x40, 0x0F, 0x00, 0x00];
pub const DATA_RATE_BPS: [u8; 4] = [0x00, 0x2A, 0x00, 0x00];

pub use ccid_firmware_rs::ccid_core::SlotState;
pub use ccid_firmware_rs::driver::SmartcardDriver;
pub use ccid_protocol::types::{
    CCID_ERR_CMD_ABORTED, CCID_ERR_CMD_NOT_SUPPORTED, CCID_ERR_CMD_SLOT_BUSY, CCID_ERR_HW_ERROR,
    CCID_ERR_ICC_MUTE, CCID_ERR_PIN_CANCELLED, CCID_ERR_PIN_TIMEOUT, COMMAND_STATUS_FAILED,
    COMMAND_STATUS_NO_ERROR, COMMAND_STATUS_TIME_EXTENSION, ICC_STATUS_NO_ICC,
    ICC_STATUS_PRESENT_ACTIVE, ICC_STATUS_PRESENT_INACTIVE, PC_TO_RDR_ABORT, PC_TO_RDR_ESCAPE,
    PC_TO_RDR_GET_PARAMETERS, PC_TO_RDR_GET_SLOT_STATUS, PC_TO_RDR_ICC_CLOCK,
    PC_TO_RDR_ICC_POWER_OFF, PC_TO_RDR_ICC_POWER_ON, PC_TO_RDR_MECHANICAL, PC_TO_RDR_SECURE,
    PC_TO_RDR_SET_DATA_RATE_AND_CLOCK_FREQ, PC_TO_RDR_SET_PARAMETERS, PC_TO_RDR_T0_APDU,
    PC_TO_RDR_XFR_BLOCK, RDR_TO_PC_DATABLOCK, RDR_TO_PC_DATA_RATE_AND_CLOCK_FREQ, RDR_TO_PC_ESCAPE,
    RDR_TO_PC_NOTIFY_SLOT_CHANGE, RDR_TO_PC_PARAMETERS, RDR_TO_PC_SLOTSTATUS,
};

/// USB CCID Class — wraps CcidMessageHandler with USB transport
pub struct CcidClass<'bus, Bus: UsbBus, D: SmartcardDriver> {
    interface: InterfaceNumber,
    ep_in: EndpointIn<'bus, Bus>,
    ep_out: EndpointOut<'bus, Bus>,
    core: CcidMessageHandler<D>,
    ep_int: EndpointIn<'bus, Bus>,
    tx_offset: usize,
    tx_pending: bool,
    needs_zlp: bool,
}

impl<'bus, Bus: UsbBus, D: SmartcardDriver> CcidClass<'bus, Bus, D> {
    pub fn new(
        allocator: &'bus UsbBusAllocator<Bus>,
        driver: D,
        ep_int: EndpointIn<'bus, Bus>,
    ) -> Self {
        Self {
            interface: allocator.interface(),
            ep_in: allocator.bulk::<In>(PACKET_SIZE as u16),
            ep_out: allocator.bulk::<Out>(PACKET_SIZE as u16),
            core: CcidMessageHandler::new(driver, CURRENT_PROFILE.vendor_id),
            ep_int,
            tx_offset: 0,
            tx_pending: false,
            needs_zlp: false,
        }
    }

    pub fn driver(&self) -> &D {
        self.core.driver()
    }

    pub fn driver_mut(&mut self) -> &mut D {
        self.core.driver_mut()
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

    pub fn take_secure_params(
        &mut self,
    ) -> Option<(u8, ccid_firmware_rs::pinpad::PinVerifyParams)> {
        self.core.take_secure_params()
    }

    pub fn take_secure_modify_params(
        &mut self,
    ) -> Option<(u8, ccid_firmware_rs::pinpad::PinModifyParams)> {
        self.core.take_secure_modify_params()
    }

    pub fn complete_pin_entry(
        &mut self,
        seq: u8,
        result: ccid_firmware_rs::pinpad::PinResult,
        apdu_response: Option<&[u8]>,
    ) {
        self.core.complete_pin_entry(seq, result, apdu_response);
    }

    pub fn is_card_present(&self) -> bool {
        self.core.is_card_present()
    }

    pub fn is_card_active(&self) -> bool {
        self.core.is_card_active()
    }

    #[cfg(feature = "display")]
    pub fn set_pin_result(
        &mut self,
        seq: u8,
        result: ccid_firmware_rs::pinpad::PinResult,
        buffer: ccid_firmware_rs::pinpad::PinBuffer,
        params: ccid_firmware_rs::pinpad::PinVerifyParams,
    ) {
        self.core.set_pin_result(seq, result, buffer, params);
    }

    #[cfg(feature = "display")]
    pub fn process_pin_result(&mut self) {
        self.core.process_pin_result();
    }

    #[cfg(feature = "display")]
    pub fn set_pin_modify_result(
        &mut self,
        seq: u8,
        result: ccid_firmware_rs::pinpad::PinResult,
        old_buffer: ccid_firmware_rs::pinpad::PinBuffer,
        new_buffer: ccid_firmware_rs::pinpad::PinBuffer,
        params: ccid_firmware_rs::pinpad::PinModifyParams,
    ) {
        self.core
            .set_pin_modify_result(seq, result, old_buffer, new_buffer, params);
    }

    #[cfg(feature = "display")]
    pub fn process_pin_modify_result(&mut self) {
        self.core.process_pin_modify_result();
    }

    fn send_notify_slot_change(&mut self, card_present: bool, changed: bool) {
        let msg = self.core.notify_slot_change_bytes(card_present, changed);
        let _ = self.ep_int.write(&msg);
    }

    fn try_send(&mut self) {
        if self.needs_zlp {
            match self.ep_in.write(&[]) {
                Ok(_) => {
                    self.needs_zlp = false;
                }
                Err(UsbError::WouldBlock) => {}
                Err(_) => {
                    self.needs_zlp = false;
                }
            }
            return;
        }
        if !self.tx_pending {
            return;
        }
        let tx_data = self.core.get_tx_buffer();
        let remaining = tx_data.len() - self.tx_offset;
        let chunk_size = remaining.min(PACKET_SIZE);
        let chunk = &tx_data[self.tx_offset..self.tx_offset + chunk_size];
        match self.ep_in.write(chunk) {
            Ok(n) => {
                self.tx_offset += n;
                if self.tx_offset >= tx_data.len() {
                    if tx_data.len() % PACKET_SIZE == 0 {
                        self.needs_zlp = true;
                    }
                    self.tx_pending = false;
                    self.tx_offset = 0;
                    self.core.take_response();
                }
            }
            Err(UsbError::WouldBlock) => {}
            Err(_) => {
                self.tx_pending = false;
                self.tx_offset = 0;
                self.core.take_response();
            }
        }
    }
}

impl<'bus, Bus: UsbBus, D: SmartcardDriver> UsbClass<Bus> for CcidClass<'bus, Bus, D> {
    fn get_configuration_descriptors(&self, writer: &mut DescriptorWriter) -> Result<()> {
        writer.interface_alt(
            self.interface,
            0,
            CURRENT_PROFILE.interface_class,
            SUBCLASS_NONE,
            PROTOCOL_BULK,
            None,
        )?;

        writer.write(DESCRIPTOR_TYPE_CCID, &CCID_CLASS_DESCRIPTOR_DATA)?;

        writer.endpoint(&self.ep_in)?;
        writer.endpoint(&self.ep_out)?;
        writer.endpoint(&self.ep_int)?;

        Ok(())
    }

    fn poll(&mut self) {
        if let Some(true) = self.core.check_card_presence() {
            self.send_notify_slot_change(self.core.is_card_present(), true);
        }

        self.try_send();

        let mut temp_buf = [0u8; PACKET_SIZE];
        match self.ep_out.read(&mut temp_buf) {
            Ok(len) => {
                defmt::info!("CCID: USB received {} bytes: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                    len,
                    temp_buf[0], temp_buf[1], temp_buf[2], temp_buf[3],
                    temp_buf[4], temp_buf[5], temp_buf[6], temp_buf[7],
                    temp_buf[8], temp_buf[9]);
                self.core.feed(&temp_buf[..len]);
                if self.core.message_ready() {
                    self.core.handle_message();
                    self.tx_pending = true;
                    self.tx_offset = 0;
                }
            }
            Err(UsbError::WouldBlock) => {}
            Err(_e) => {
                defmt::error!("CCID: read error");
            }
        }

        self.try_send();
    }

    fn endpoint_out(&mut self, addr: EndpointAddress) {
        if addr == self.ep_out.address() {}
    }

    fn endpoint_in_complete(&mut self, addr: EndpointAddress) {
        if addr == self.ep_in.address() {
            self.try_send();
        }
    }

    fn control_in(&mut self, transfer: ControlIn<Bus>) {
        let request = transfer.request();

        if request.index as u8 != u8::from(self.interface) {
            return;
        }

        if request.request_type != usb_device::control::RequestType::Class {
            return;
        }

        match request.request {
            REQUEST_GET_CLOCK_FREQUENCIES => {
                transfer.accept_with(&CLOCK_FREQUENCY_KHZ).ok();
            }
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

        if request.index as u8 != u8::from(self.interface) {
            return;
        }

        if request.request_type != usb_device::control::RequestType::Class {
            return;
        }

        match request.request {
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
