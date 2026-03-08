// Patch for STM32F469 USB OTG support
// The F469 has core_id 0x0000_1200 but needs F446-like endpoint re-enable behavior

use crate::ral::{modify_reg, otg_global, otg_device, endpoint_out, read_reg, write_reg};
use crate::target::UsbRegisters;

const STM32F469_CORE_ID: u32 = 0x0000_1200;

pub fn is_f469_like(core_id: u32) -> bool {
    core_id == STM32F469_CORE_ID
}

pub fn re_enable_endpoint_out(usb: UsbRegisters, epnum: u8) {
    let ep = usb.endpoint_out(epnum as usize);
    modify_reg!(endpoint_out, ep, DOEPCTL, CNAK: 1, EPENA: 1);
}
